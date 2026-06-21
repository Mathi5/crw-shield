// tls-impersonate-proxy — HTTPS MITM proxy that re-issues requests
// through bogdanfinn/tls-client with an impersonated browser fingerprint.
//
// Chrome is configured with --proxy-server=http://127.0.0.1:PORT and
// --ignore-certificate-errors (so it trusts our dynamically generated
// per-host certs).
//
// Flow:
//   1. Chrome → CONNECT target:443
//   2. We ACK with 200
//   3. TLS handshake with Chrome using a cert for target signed by our local CA
//   4. Chrome → HTTP request (decrypted, lives inside our TLS tunnel)
//   5. We re-issue to the real target via tls-client (impersonated fingerprint)
//   6. We stream the response back to Chrome (re-encrypted)
//
// Compile:
//   CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o tls-impersonate-proxy .
//
// Originally from CyrilLeblanc/cortex-bridge (MIT, 2026).
// Integrated into crw-shield as a sidecar binary under the same MIT license.
package main

import (
	"bufio"
	"crypto/rand"
	"crypto/rsa"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"flag"
	"fmt"
	"io"
	"log"
	"math/big"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	fhttp "github.com/bogdanfinn/fhttp"
	tlsclient "github.com/bogdanfinn/tls-client"
	"github.com/bogdanfinn/tls-client/profiles"
)

var (
	listenAddr  = flag.String("listen", "127.0.0.1:7890", "listen address")
	profileName = flag.String("profile", "chrome_120", "browser profile: chrome_120, chrome_117, chrome_116_psk, chrome_110, firefox_117, safari_16_0")
	caDir       = flag.String("ca-dir", "", "directory to store/load CA cert+key (persistent across runs)")
	caCertOut   = flag.String("ca-cert-out", "", "write the CA cert (PEM) to this file path")
	timeout     = flag.Duration("timeout", 60*time.Second, "per-request timeout")
	maxBody     = flag.Int64("max-body", 16*1024*1024, "max request body bytes (16 MB default)")
	bypassList  = flag.String("bypass", "localhost,127.0.0.1,::1", "comma-separated hosts to forward as-is (raw tunnel, no MITM)")
	quietLogs   = flag.Bool("quiet", false, "suppress per-request logs (errors only)")
)

const (
	rsaBits        = 2048
	caOrgName      = "CortexBridge TLS Impersonation Proxy"
	caCommonName   = "tls-impersonate-proxy CA"
	leafCommonName = "tls-impersonate-proxy"
)

func main() {
	flag.Parse()

	ca, err := loadOrCreateCA(*caDir)
	if err != nil {
		log.Fatalf("CA: %v", err)
	}
	if *caCertOut != "" {
		if err := os.WriteFile(*caCertOut, pemCert(ca.tls.Certificate[0]), 0644); err != nil {
			log.Printf("warning: write CA cert to %s: %v", *caCertOut, err)
		} else {
			log.Printf("CA cert written to %s", *caCertOut)
		}
	}

	idleTimeout := 90 * time.Second
	client, err := tlsclient.NewHttpClient(nil,
		tlsclient.WithTimeoutSeconds(int(timeout.Seconds())),
		tlsclient.WithClientProfile(resolveProfile(*profileName)),
		tlsclient.WithNotFollowRedirects(),
		tlsclient.WithDefaultHeaders(chromeDefaultHeaders()),
		// Pool tuning: fhttp defaults to MaxIdleConnsPerHost=2 which is too low
		// for scraping sites that fan out 20+ requests to the same host
		// (fonts/CSS/JS chunks on the same CDN). Bump to 16 idle per host,
		// 100 total, 90s idle timeout.
		tlsclient.WithTransportOptions(&tlsclient.TransportOptions{
			MaxIdleConns:          100,
			MaxIdleConnsPerHost:   16,
			MaxConnsPerHost:       0,
			IdleConnTimeout:       &idleTimeout,
			DisableKeepAlives:     false,
			DisableCompression:    true,
		}),
	)
	if err != nil {
		log.Fatalf("create tls-client: %v", err)
	}

	ln, err := net.Listen("tcp", *listenAddr)
	if err != nil {
		log.Fatalf("listen %s: %v", *listenAddr, err)
	}
	defer ln.Close()

	if *quietLogs {
		log.SetOutput(io.Discard)
	}
	log.SetFlags(log.Lmicroseconds | log.Lmsgprefix)
	log.Printf("tls-impersonate-proxy listening on %s profile=%s timeout=%s bypass=%q", *listenAddr, *profileName, *timeout, *bypassList)
	if *caDir != "" {
		log.Printf("CA: dir=%s (cert=%s, key=%s)", *caDir, filepath.Join(*caDir, "ca.crt"), filepath.Join(*caDir, "ca.key"))
	}

	var id uint64
	for {
		conn, err := ln.Accept()
		if err != nil {
			if !isClosedErr(err) {
				log.Printf("accept: %v", err)
			}
			continue
		}
		atomic.AddUint64(&id, 1)
		go handleConnection(conn, client, ca, atomic.LoadUint64(&id))
	}
}

func handleConnection(conn net.Conn, client tlsclient.HttpClient, ca *ca, id uint64) {
	defer conn.Close()

	conn.SetDeadline(time.Now().Add(*timeout))
	reader := bufio.NewReader(conn)
	logf := func(format string, args ...interface{}) {
		if !*quietLogs {
			log.Printf("[%d] "+format, append([]interface{}{id}, args...)...)
		}
	}

	// Read request line
	line, err := reader.ReadString('\n')
	if err != nil {
		if err != io.EOF {
			logf("read line: %v", err)
		}
		return
	}
	line = strings.TrimRight(line, "\r\n")
	parts := strings.SplitN(line, " ", 3)
	if len(parts) < 3 {
		logf("malformed request line: %q", line)
		sendProxyError(conn, 400, "malformed request line")
		return
	}
	method, target, _ := parts[0], parts[1], parts[2]

	// Read headers (we don't need them for method dispatch, but must consume them)
	hdr := http.Header{}
	for {
		hl, err := reader.ReadString('\n')
		if err != nil {
			return
		}
		hl = strings.TrimRight(hl, "\r\n")
		if hl == "" {
			break
		}
		c := strings.Index(hl, ":")
		if c < 0 {
			continue
		}
		k := http.CanonicalHeaderKey(strings.TrimSpace(hl[:c]))
		v := strings.TrimSpace(hl[c+1:])
		hdr.Add(k, v)
	}

	// Reset deadline — CONNECT is one logical unit but may take longer
	conn.SetDeadline(time.Now().Add(*timeout))

	switch {
	case method == "CONNECT":
		hostPort := target
		if !strings.Contains(hostPort, ":") {
			hostPort += ":443"
		}
		if shouldBypass(hostPort, *bypassList) {
			logf("CONNECT %s (bypass, raw tunnel)", hostPort)
			if err := rawTunnel(conn, hostPort); err != nil {
				logf("bypass tunnel: %v", err)
			}
			return
		}
		logf("CONNECT %s (MITM)", hostPort)
		if err := handleMITM(conn, hostPort, client, ca, logf); err != nil {
			logf("MITM %s: %v", hostPort, err)
		}

	case strings.HasPrefix(target, "http://") || strings.HasPrefix(target, "https://"):
		logf("HTTP %s %s", method, target)
		if err := handleHTTP(conn, method, target, hdr, client, logf); err != nil {
			logf("HTTP %s %s: %v", method, target, err)
		}

	default:
		logf("unsupported proxy request: %s %s", method, target)
		sendProxyError(conn, 400, "unsupported proxy request")
	}
}

// handleMITM — for HTTPS targets. Terminate Chrome's TLS, decrypt the request,
// re-issue via tls-client with impersonated fingerprint, encrypt the response.
func handleMITM(conn net.Conn, hostPort string, client tlsclient.HttpClient, ca *ca, logf func(string, ...interface{})) error {
	// Acknowledge the tunnel
	if _, err := conn.Write([]byte("HTTP/1.1 200 Connection Established\r\n\r\n")); err != nil {
		return err
	}

	// Dynamically generate a cert for hostPort
	leafCert, err := generateLeafCert(hostPort, ca)
	if err != nil {
		return fmt.Errorf("generate cert: %w", err)
	}
	tlsConfig := &tls.Config{
		Certificates: []tls.Certificate{leafCert},
		MinVersion:   tls.VersionTLS12,
	}

	// Wrap conn in TLS
	tlsConn := tls.Server(conn, tlsConfig)
	if err := tlsConn.Handshake(); err != nil {
		return fmt.Errorf("TLS handshake with client: %w", err)
	}
	defer tlsConn.Close()

	// Reset deadline
	tlsConn.SetDeadline(time.Now().Add(*timeout))

	// Now read HTTP request from Chrome (decrypted)
	for {
		req, err := http.ReadRequest(bufio.NewReader(tlsConn))
		if err != nil {
			if err == io.EOF {
				return nil
			}
			return err
		}

		// Re-issue via tls-client
		fullURL := "https://" + hostPort + req.URL.RequestURI()
		fReq, err := fhttp.NewRequest(req.Method, fullURL, req.Body)
		if err != nil {
			sendMITMError(tlsConn, 502, "bad request: "+err.Error())
			return err
		}
		for k, v := range req.Header {
			if isHopByHop(k) {
				continue
			}
			// Strip Chrome's UA Client Hints — they reflect the actual Chromium
			// binary version (e.g. 149), not the impersonation target (e.g. 120).
			// We re-inject values matching the TLS profile below.
			if isUACH(k) {
				continue
			}
			// Strip headers that tls-client sets from its profile defaults.
			// Chrome sends its own values for these (real binary version, real
			// locale, no br-accept for some traffic), and forwarding both copies
			// produces a duplicate-header signature that anti-bots like Zillow's
			// flag as a proxy artefact.
			if isChromeOverridden(k) {
				continue
			}
			for _, vv := range v {
				fReq.Header.Add(k, vv)
			}
		}
		overrideUACH(fReq, *profileName)
		// Merge profile defaults for headers that were stripped by isChromeOverridden.
		// tls-client's WithDefaultHeaders only applies when len(req.Header)==0
		// (tls-client@v1.7.4/client.go:298). Since we always set Cookie + Sec-Ch-Ua*
		// + Upgrade-Insecure-Requests above, the header map is never empty, so
		// User-Agent, Accept, Accept-Encoding, Accept-Language, Sec-Fetch-* would be
		// silently dropped — sending Zillow a request with no UA and getting a
		// hard 403. Merge the defaults explicitly here instead.
		// Canonicalize keys because the default maps mix case (e.g. "sec-ch-ua"
		// vs "Sec-Ch-Ua") while fhttp stores them canonically.
		var defaults fhttp.Header
		if isFirefoxProfile(*profileName) {
			defaults = firefoxDefaultHeaders()
		} else {
			defaults = chromeDefaultHeaders()
		}
		for k, vv := range defaults {
			ck := http.CanonicalHeaderKey(k)
			if _, exists := fReq.Header[ck]; !exists {
				fReq.Header.Set(ck, vv[0])
			}
		}
		fReq.Host = hostPort
		fReq.ContentLength = req.ContentLength

		// DEBUG: log headers being sent
		if !*quietLogs {
			logf("  headers (in fhttp Header map; H1 order is random, H2 is alphabetical):")
			for k, v := range fReq.Header {
				logf("    %s: %s", k, v)
			}
		}

		logf("→ %s %s", req.Method, fullURL)
		fResp, err := client.Do(fReq)
		if err != nil {
			logf("↑ %s %s: %v", req.Method, fullURL, err)
			sendMITMError(tlsConn, 502, "upstream error: "+err.Error())
			return err
		}
		if !*quietLogs {
			logf("  upstream resp: %d %s, content-length=%d", fResp.StatusCode, http.StatusText(fResp.StatusCode), fResp.ContentLength)
		}

		if err := writeMITMResponseWithLog(tlsConn, fResp, logf); err != nil {
			fResp.Body.Close()
			return err
		}
		fResp.Body.Close()
		logf("← %s %s %d", req.Method, fullURL, fResp.StatusCode)

		if strings.EqualFold(req.Header.Get("Connection"), "close") {
			return nil
		}
	}
}

// handleHTTP — for plain HTTP targets, re-issue via tls-client.
func handleHTTP(conn net.Conn, method, target string, hdr http.Header, client tlsclient.HttpClient, logf func(string, ...interface{})) error {
	u, err := url.Parse(target)
	if err != nil {
		sendProxyError(conn, 400, "bad URL")
		return err
	}

	var body io.Reader
	if cl := hdr.Get("Content-Length"); cl != "" {
		n, err := strconv.ParseInt(cl, 10, 64)
		if err != nil || n < 0 {
			return fmt.Errorf("bad content-length")
		}
		if n > *maxBody {
			return fmt.Errorf("body too large")
		}
		body = io.LimitReader(conn, n)
	}

	fReq, err := fhttp.NewRequest(method, target, body)
	if err != nil {
		sendProxyError(conn, 400, err.Error())
		return err
	}
	for k, v := range hdr {
		if isHopByHop(k) {
			continue
		}
		for _, vv := range v {
			fReq.Header.Add(k, vv)
		}
	}
	// Merge profile defaults (same reason as handleMITM: tls-client's
	// WithDefaultHeaders is skipped when the header map is non-empty).
	// handleHTTP targets aren't normally behind aggressive anti-bot, but
	// keeping the merge here makes the two paths consistent.
	var defaults fhttp.Header
	if isFirefoxProfile(*profileName) {
		defaults = firefoxDefaultHeaders()
	} else {
		defaults = chromeDefaultHeaders()
	}
	for k, vv := range defaults {
		ck := http.CanonicalHeaderKey(k)
		if _, exists := fReq.Header[ck]; !exists {
			fReq.Header.Set(ck, vv[0])
		}
	}
	fReq.Host = u.Host

	logf("→ %s %s", method, target)
	fResp, err := client.Do(fReq)
	if err != nil {
		sendProxyError(conn, 502, err.Error())
		return err
	}
	defer fResp.Body.Close()

	if err := writeProxyResponse(conn, fResp); err != nil {
		return err
	}
	logf("← %s %s %d", method, target, fResp.StatusCode)
	return nil
}

// rawTunnel — for bypass hosts, raw TCP tunnel (no impersonation).
func rawTunnel(conn net.Conn, hostPort string) error {
	upstream, err := net.DialTimeout("tcp", hostPort, 10*time.Second)
	if err != nil {
		sendProxyError(conn, 502, "dial upstream: "+err.Error())
		return err
	}
	defer upstream.Close()

	done := make(chan struct{}, 2)
	go func() {
		io.Copy(upstream, conn)
		done <- struct{}{}
	}()
	go func() {
		io.Copy(conn, upstream)
		done <- struct{}{}
	}()
	<-done
	return nil
}

func writeMITMResponse(conn net.Conn, resp *fhttp.Response) error {
	return writeMITMResponseWithLog(conn, resp, func(string, ...interface{}) {})
}

func writeMITMResponseWithLog(conn net.Conn, resp *fhttp.Response, logf func(string, ...interface{})) error {
	statusLine := fmt.Sprintf("HTTP/1.1 %d %s\r\n", resp.StatusCode, http.StatusText(resp.StatusCode))
	if _, err := conn.Write([]byte(statusLine)); err != nil {
		return err
	}
	for k, v := range resp.Header {
		if isHopByHop(k) {
			continue
		}
		for _, vv := range v {
			if _, err := conn.Write([]byte(fmt.Sprintf("%s: %s\r\n", k, vv))); err != nil {
				return err
			}
		}
	}
	// Read raw upstream body (still compressed) and forward as-is.
	// Chrome handles decompression natively (gzip, brotli, zstd, …) so we
	// keep the original Content-Encoding + Content-Length headers above.
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return err
	}
	resp.Body.Close()
	if _, err := fmt.Fprintf(conn, "Content-Length: %d\r\n", len(body)); err != nil {
		return err
	}
	if _, err := conn.Write([]byte("\r\n")); err != nil {
		return err
	}
	if _, err := conn.Write(body); err != nil {
		return err
	}
	return nil
}

func writeProxyResponse(conn net.Conn, resp *fhttp.Response) error {
	return writeMITMResponse(conn, resp) // same logic
}

func sendProxyError(conn net.Conn, code int, msg string) {
	body := msg
	fmt.Fprintf(conn, "HTTP/1.1 %d %s\r\nContent-Type: text/plain\r\nContent-Length: %d\r\nConnection: close\r\n\r\n%s",
		code, http.StatusText(code), len(body), body)
}

func sendMITMError(conn net.Conn, code int, msg string) {
	body := msg
	fmt.Fprintf(conn, "HTTP/1.1 %d %s\r\nContent-Type: text/plain\r\nContent-Length: %d\r\nConnection: close\r\n\r\n%s",
		code, http.StatusText(code), len(body), body)
}

// =============================================================================
// CA + per-host cert generation
// =============================================================================

type ca struct {
	cert   *x509.Certificate
	key    *rsa.PrivateKey
	tls    tls.Certificate
	certPEM []byte
}

func loadOrCreateCA(dir string) (*ca, error) {
	if dir != "" {
		if err := os.MkdirAll(dir, 0700); err != nil {
			return nil, err
		}
		certPath := filepath.Join(dir, "ca.crt")
		keyPath := filepath.Join(dir, "ca.key")
		if _, err := os.Stat(certPath); err == nil {
			if _, err := os.Stat(keyPath); err == nil {
				return loadCA(certPath, keyPath)
			}
		}
	}
	return createCA(dir)
}

func loadCA(certPath, keyPath string) (*ca, error) {
	certPEM, err := os.ReadFile(certPath)
	if err != nil {
		return nil, err
	}
	keyPEM, err := os.ReadFile(keyPath)
	if err != nil {
		return nil, err
	}
	tlsCert, err := tls.X509KeyPair(certPEM, keyPEM)
	if err != nil {
		return nil, err
	}
	x509Cert, err := x509.ParseCertificate(tlsCert.Certificate[0])
	if err != nil {
		return nil, err
	}
	rsaKey, ok := tlsCert.PrivateKey.(*rsa.PrivateKey)
	if !ok {
		return nil, fmt.Errorf("CA key is not RSA")
	}
	return &ca{cert: x509Cert, key: rsaKey, tls: tlsCert, certPEM: certPEM}, nil
}

func createCA(dir string) (*ca, error) {
	key, err := rsa.GenerateKey(rand.Reader, rsaBits)
	if err != nil {
		return nil, err
	}
	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(time.Now().Unix()),
		Subject: pkix.Name{
			CommonName:   caCommonName,
			Organization: []string{caOrgName},
		},
		NotBefore:             time.Now().Add(-24 * time.Hour),
		NotAfter:              time.Now().Add(10 * 365 * 24 * time.Hour),
		IsCA:                  true,
		KeyUsage:              x509.KeyUsageCertSign | x509.KeyUsageDigitalSignature,
		BasicConstraintsValid: true,
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		return nil, err
	}
	tlsCert := tls.Certificate{
		Certificate: [][]byte{der},
		PrivateKey:  key,
	}
	certPEM := pemCert(der)
	keyPEM := pemPKCS1Key(key)

	if dir != "" {
		_ = os.WriteFile(filepath.Join(dir, "ca.crt"), certPEM, 0644)
		_ = os.WriteFile(filepath.Join(dir, "ca.key"), keyPEM, 0600)
	}

	return &ca{cert: tmpl, key: key, tls: tlsCert, certPEM: certPEM}, nil
}

func generateLeafCert(hostPort string, ca *ca) (tls.Certificate, error) {
	host, _, err := net.SplitHostPort(hostPort)
	if err != nil {
		host = hostPort
	}

	key, err := rsa.GenerateKey(rand.Reader, rsaBits)
	if err != nil {
		return tls.Certificate{}, err
	}

	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(time.Now().UnixNano()),
		Subject: pkix.Name{
			CommonName:   host,
			Organization: []string{caOrgName},
		},
		NotBefore:   time.Now().Add(-time.Hour),
		NotAfter:    time.Now().Add(24 * time.Hour),
		KeyUsage:    x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment,
		ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	}
	if ip := net.ParseIP(host); ip != nil {
		tmpl.IPAddresses = []net.IP{ip}
	} else {
		tmpl.DNSNames = []string{host}
	}

	der, err := x509.CreateCertificate(rand.Reader, tmpl, ca.cert, &key.PublicKey, ca.key)
	if err != nil {
		return tls.Certificate{}, err
	}
	return tls.Certificate{
		Certificate: [][]byte{der, ca.tls.Certificate[0]},
		PrivateKey:  key,
	}, nil
}

func pemCert(der []byte) []byte {
	return pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})
}

func pemPKCS1Key(key *rsa.PrivateKey) []byte {
	return pem.EncodeToMemory(&pem.Block{Type: "RSA PRIVATE KEY", Bytes: x509.MarshalPKCS1PrivateKey(key)})
}

// =============================================================================
// Default headers — match Chrome's typical HTTP/1.1 request signature.
// Many anti-bots check for the presence and ORDER of these headers; we set
// them in roughly the same order Chrome uses (tls-client merges them in
// Go map iteration order, but the values are correct).
// =============================================================================

func chromeDefaultHeaders() fhttp.Header {
	return fhttp.Header{
		"User-Agent":                {"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"},
		"sec-ch-ua":                 {`"Not_A Brand";v="8", "Chromium";v="120", "Google Chrome";v="120"`},
		"sec-ch-ua-mobile":          {"?0"},
		"sec-ch-ua-platform":        {`"Linux"`},
		"Upgrade-Insecure-Requests": {"1"},
		"Accept":                    {"text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"},
		"Accept-Encoding":           {"gzip, deflate, br"},
		"Accept-Language":           {"en-US,en;q=0.9"},
		"sec-fetch-mode":            {"navigate"},
		"sec-fetch-site":            {"none"},
		"sec-fetch-user":            {"?1"},
		"sec-fetch-dest":            {"document"},
	}
}

func firefoxDefaultHeaders() fhttp.Header {
	return fhttp.Header{
		"User-Agent":      {"Mozilla/5.0 (X11; Linux x86_64; rv:123.0) Gecko/20100101 Firefox/123.0"},
		"Accept":          {"text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"},
		"Accept-Encoding": {"gzip, deflate, br"},
		"Accept-Language": {"en-US,en;q=0.5"},
		"sec-fetch-mode":  {"navigate"},
		"sec-fetch-site":  {"none"},
		"sec-fetch-user":  {"?1"},
		"sec-fetch-dest":  {"document"},
	}
}

func isFirefoxProfile(profile string) bool {
	return strings.HasPrefix(profile, "firefox_")
}

func resolveProfile(name string) profiles.ClientProfile {
	switch strings.ToLower(name) {
	case "chrome_120", "chrome-120":
		return profiles.Chrome_120
	case "chrome_117", "chrome-117":
		return profiles.Chrome_117
	case "chrome_116_psk":
		return profiles.Chrome_116_PSK
	case "chrome_116_psk_pq":
		return profiles.Chrome_116_PSK_PQ
	case "chrome_112":
		return profiles.Chrome_112
	case "chrome_110", "chrome-110":
		return profiles.Chrome_110
	case "chrome_107", "chrome-107":
		return profiles.Chrome_107
	case "firefox_123":
		return profiles.Firefox_123
	case "firefox_120":
		return profiles.Firefox_120
	case "firefox_117":
		return profiles.Firefox_117
	case "firefox_110":
		return profiles.Firefox_110
	case "safari_16_0", "safari-16-0":
		return profiles.Safari_16_0
	case "safari_15_6_1":
		return profiles.Safari_15_6_1
	}
	log.Printf("unknown profile %q, falling back to chrome_120", name)
	return profiles.Chrome_120
}

func isHopByHop(k string) bool {
	switch k {
	case "Connection", "Keep-Alive", "Proxy-Authenticate", "Proxy-Authorization",
		"Te", "Trailer", "Transfer-Encoding", "Upgrade":
		return true
	}
	return false
}

// isUACH returns true for User-Agent Client Hints headers that Chrome sends
// based on its actual binary version (e.g. Chromium 149). These leak the real
// version independently of --user-agent and would mismatch our impersonation
// target (e.g. chrome_120). Strip them and re-inject via overrideUACH.
func isUACH(k string) bool {
	switch k {
	case "Sec-Ch-Ua",
		"Sec-Ch-Ua-Mobile",
		"Sec-Ch-Ua-Platform",
		"Sec-Ch-Ua-Arch",
		"Sec-Ch-Ua-Model",
		"Sec-Ch-Ua-Bitness",
		"Sec-Ch-Ua-Full-Version-List",
		"Sec-Ch-Ua-Platform-Version",
		"Sec-Ch-Ua-WoW64",
		"Sec-Ch-Ua-Form-Factors",
		"Sec-Ch-Prefers-Color-Scheme",
		"Sec-Ch-Viewport-Width",
		"Sec-Ch-Viewport-Height",
		"Sec-Ch-Dpr",
		"Sec-Ch-Device-Memory",
		"Sec-Ch-Rtt",
		"Sec-Ch-Downlink",
		"Sec-Ch-Ect",
		"Sec-Ch-Save-Data",
		"Sec-Ch-Prefers-Reduced-Motion",
		"Sec-Ch-Prefers-Reduced-Transparency",
		"Sec-Ch-Prefers-Contrast",
		"Sec-Ch-Forced-Colors":
		return true
	}
	return false
}

// isChromeOverridden returns true for headers that tls-client already provides
// via chromeDefaultHeaders() (matched to the impersonation profile). Letting
// Chrome's copy through produces duplicate-header requests that some
// anti-bot stacks (notably Zillow) treat as a proxy signature. The defaults
// re-injected from the profile are correct for the impersonated UA.
func isChromeOverridden(k string) bool {
	switch k {
	case "User-Agent",
		"Accept",
		"Accept-Encoding",
		"Accept-Language":
		return true
	}
	if strings.HasPrefix(k, "Sec-Fetch-") {
		return true
	}
	return false
}

// overrideUACH replaces Sec-Ch-Ua* on the upstream request with values matching
// the impersonated profile. Without this, Chrome 149 sends
// `Sec-Ch-Ua: "Chromium";v="149", "Not)A;Brand";v="24"` while we negotiate a
// chrome_120 TLS fingerprint — AWS WAF / Datadome flag this mismatch.
func overrideUACH(req *fhttp.Request, profile string) {
	if isFirefoxProfile(profile) {
		return // Firefox ne pas envoyer Sec-Ch-Ua*
	}
	// Default values match Chrome 120 on Linux x86_64.
	secChUa := `"Not_A Brand";v="8", "Chromium";v="120", "Google Chrome";v="120"`
	secChUaMobile := "?0"
	secChUaPlatform := `"Linux"`
	secChUaArch := `"x86"`

	if strings.HasPrefix(profile, "chrome_") {
		// Pull major version from profile name (chrome_120 → 120)
		if v := strings.TrimPrefix(profile, "chrome_"); v != "" {
			// Strip suffixes like _psk, _pq
			if idx := strings.IndexAny(v, "_-"); idx > 0 {
				v = v[:idx]
			}
			if strings.HasPrefix(v, "1") || strings.HasPrefix(v, "12") || strings.HasPrefix(v, "13") {
				secChUa = fmt.Sprintf(`"Not_A Brand";v="8", "Chromium";v="%s", "Google Chrome";v="%s"`, v, v)
			} else if strings.HasPrefix(v, "11") {
				// Chrome 110-119 still used "Not_A Brand";v="99" in some configs.
				secChUa = fmt.Sprintf(`"Not_A Brand";v="99", "Chromium";v="%s", "Google Chrome";v="%s"`, v, v)
			}
		}
	}

	h := req.Header
	h.Set("Sec-Ch-Ua", secChUa)
	h.Set("Sec-Ch-Ua-Mobile", secChUaMobile)
	h.Set("Sec-Ch-Ua-Platform", secChUaPlatform)
	h.Set("Sec-Ch-Ua-Arch", secChUaArch)
}

func shouldBypass(host string, list string) bool {
	if list == "" {
		return false
	}
	hostOnly := host
	if h, _, err := net.SplitHostPort(host); err == nil {
		hostOnly = h
	}
	for _, b := range strings.Split(list, ",") {
		b = strings.TrimSpace(b)
		if b == "" {
			continue
		}
		if strings.EqualFold(b, hostOnly) || strings.EqualFold(b, host) {
			return true
		}
	}
	return false
}

func isClosedErr(err error) bool {
	if err == nil {
		return false
	}
	s := err.Error()
	return strings.Contains(s, "use of closed network connection") ||
		strings.Contains(s, "EOF")
}

// Kept to silence unused imports if refactored
var (
	_ = atomic.AddUint64
	_ = sync.Mutex{}
)
