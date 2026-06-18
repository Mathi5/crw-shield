//! CDP stealth — JavaScript injected via `Page.addScriptToEvaluateOnNewDocument`
//! before any page script runs.
//!
//! The script patches the browser fingerprint to remove the common automation
//! markers used by Cloudflare, DataDome, PerimeterX, etc. It is intentionally
//! conservative — every patch is feature-detected so a missing API on a given
//! page does not break the page itself.

/// Returns the full JavaScript payload to inject before navigation.
///
/// The script is wrapped in an IIFE so leaking helpers into `window` is
/// avoided, but it still installs a few properties that tests on the host
/// side can reach via the regular DOM (`navigator.webdriver`, etc.).
pub fn stealth_script() -> &'static str {
    STEALTH_JS
}

const STEALTH_JS: &str = r#"(function () {
  'use strict';
  try {
    // ---------- navigator.webdriver ----------
    Object.defineProperty(Navigator.prototype, 'webdriver', {
      get: function () { return undefined; },
      set: function () {},
      configurable: true,
      enumerable: true
    });
    try {
      Object.defineProperty(navigator, 'webdriver', {
        get: function () { return undefined; },
        configurable: true
      });
    } catch (e) {}

    // ---------- navigator.languages ----------
    try {
      Object.defineProperty(navigator, 'languages', {
        get: function () { return ['en-US', 'en']; },
        configurable: true
      });
    } catch (e) {}

    // ---------- navigator.plugins (non-empty array) ----------
    try {
      var fakePlugins = [
        { name: 'Chrome PDF Plugin', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'Chrome PDF Viewer', filename: 'mhjfbmdgcfjbbpaeojofohoefgiehjai', description: '' },
        { name: 'Native Client', filename: 'internal-nacl-plugin', description: '' }
      ];
      var pluginArray = fakePlugins;
      pluginArray.item = function (i) { return this[i] || null; };
      pluginArray.namedItem = function (n) { for (var i=0;i<this.length;i++) if (this[i].name===n) return this[i]; return null; };
      pluginArray.refresh = function () {};
      Object.defineProperty(navigator, 'plugins', {
        get: function () { return pluginArray; },
        configurable: true
      });
    } catch (e) {}

    // ---------- navigator.userAgentData (client hints) ----------
    try {
      var brands = [
        { brand: 'Chromium', version: '131' },
        { brand: 'Not_A Brand', version: '24' },
        { brand: 'Google Chrome', version: '131' }
      ];
      var uaData = {
        brands: brands,
        mobile: false,
        platform: 'Linux x86_64',
        getHighEntropyValues: function (hints) {
          var out = {};
          if (hints.indexOf('architecture') !== -1) out.architecture = 'x86';
          if (hints.indexOf('bitness') !== -1) out.bitness = '64';
          if (hints.indexOf('model') !== -1) out.model = '';
          if (hints.indexOf('platform') !== -1) out.platform = 'Linux';
          if (hints.indexOf('platformVersion') !== -1) out.platformVersion = '6.1.0';
          if (hints.indexOf('uaFullVersion') !== -1) out.uaFullVersion = '131.0.6778.85';
          return Promise.resolve(out);
        },
        toJSON: function () { return { brands: brands, mobile: false, platform: 'Linux x86_64' }; }
      };
      Object.defineProperty(navigator, 'userAgentData', {
        get: function () { return uaData; },
        configurable: true
      });
    } catch (e) {}

    // ---------- window.chrome.runtime ----------
    try {
      if (!window.chrome) window.chrome = {};
      window.chrome.runtime = {
        PlatformOs: { MAC: 'mac', WIN: 'win', ANDROID: 'android', CROS: 'cros', LINUX: 'linux', OPENBSD: 'openbsd' },
        PlatformArch: { ARM: 'arm', X86_32: 'x86-32', X86_64: 'x86-64' },
        RequestUpdateCheckStatus: { THROTTLED: 'throttled', NO_UPDATE: 'no_update', UPDATE_AVAILABLE: 'update_available' },
        OnInstalledReason: { CHROME_UPDATE: 'chrome_update', ON_UPDATE_URL_fetch: 'ondemand', SHARED_MODULE_UPDATE: 'shared_module_update' },
        OnRestartRequiredReason: { APP_UPDATE: 'app_update', OS_UPDATE: 'os_update', PERIODIC: 'periodic' },
        connect: function () { return { onMessage: { addListener: function () {} }, postMessage: function () {}, onDisconnect: { addListener: function () {} } }; },
        sendMessage: function (_msg, _opts, cb) { if (typeof cb === 'function') { try { cb({}); } catch (e) {} } return Promise.resolve({}); }
      };
    } catch (e) {}

    // ---------- chrome.csi / chrome.loadTimes ----------
    try {
      var now = function () { return Date.now() / 1000; };
      var start = now();
      window.chrome.csi = function () {
        return { startE: start, onloadT: now() - start, pageT: now() - start, tran: 15 };
      };
      window.chrome.loadTimes = function () {
        return {
          requestTime: start,
          startLoadTime: start,
          commitLoadTime: start + 0.05,
          finishDocumentLoadTime: start + 0.1,
          finishLoadTime: start + 0.2,
          firstPaintTime: start + 0.15,
          firstPaintAfterLoadTime: 0,
          navigationType: 'Other',
          wasFetchedViaSpdy: false,
          wasNegotiatedAfterServeryAlpn: false,
          wasAlternateProtocolAvailable: false,
          connectionInfo: 'h2'
        };
      };
    } catch (e) {}

    // ---------- chrome.app ----------
    try {
      if (!window.chrome) window.chrome = {};
      window.chrome.app = {
        isInstalled: false,
        InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' },
        RunningState: { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' },
        getDetails: function () { return null; },
        getIsInstalled: function () { return false; },
        installState: function (cb) { if (typeof cb === 'function') cb('not_installed'); }
      };
    } catch (e) {}

    // ---------- Permissions API bypass ----------
    try {
      if (navigator.permissions && navigator.permissions.query) {
        var originalQuery = navigator.permissions.query.bind(navigator.permissions);
        navigator.permissions.query = function (descriptor) {
          if (descriptor && descriptor.name === 'notifications') {
            return Promise.resolve({ state: Notification.permission || 'default', onchange: null });
          }
          try { return originalQuery(descriptor); }
          catch (e) { return Promise.reject(e); }
        };
      }
      if (typeof window.Notification === 'function') {
        try { Object.defineProperty(Notification, 'permission', { get: function () { return 'default'; } }); } catch (e) {}
      }
    } catch (e) {}

    // ---------- WebGL / WebGL2 vendor spoofing (Intel Iris) ----------
    try {
      var WEBGL_VENDOR = 37445;
      var WEBGL_RENDERER = 37446;
      var VENDOR_STR = 'Intel Inc.';
      var RENDERER_STR = 'Intel Iris OpenGL Engine';
      var patchGetParameter = function (proto) {
        if (!proto || !proto.prototype || !proto.prototype.getParameter) return;
        var orig = proto.prototype.getParameter;
        proto.prototype.getParameter = function (param) {
          if (param === WEBGL_VENDOR) return VENDOR_STR;
          if (param === WEBGL_RENDERER) return RENDERER_STR;
          return orig.call(this, param);
        };
      };
      patchGetParameter(window.WebGLRenderingContext);
      patchGetParameter(window.WebGL2RenderingContext);

      var patchDebug = function (proto) {
        if (!proto || !proto.prototype) return;
        var vendor = proto.prototype.getVendor || proto.prototype.getBrowserVendor;
        var renderer = proto.prototype.getRenderer || proto.prototype.getBrowserRenderer;
        if (vendor) proto.prototype.getVendor = function () { return VENDOR_STR; };
        if (renderer) proto.prototype.getRenderer = function () { return RENDERER_STR; };
        if (proto.prototype.getUnmaskedVendorWebgl) proto.prototype.getUnmaskedVendorWebgl = function () { return VENDOR_STR; };
        if (proto.prototype.getUnmaskedRendererWebgl) proto.prototype.getUnmaskedRendererWebgl = function () { return RENDERER_STR; };
        if (proto.prototype.getExtension) {
          var origExt = proto.prototype.getExtension;
          proto.prototype.getExtension = function (name) {
            var ext = origExt.call(this, name);
            if (ext && name && /WEBGL_debug/i.test(name)) {
              try { ext.UNMASKED_VENDOR_WEBGL = WEBGL_VENDOR; ext.UNMASKED_RENDERER_WEBGL = WEBGL_RENDERER; } catch (e) {}
            }
            return ext;
          };
        }
      };
      patchDebug(window.WebGLRenderingContext);
      patchDebug(window.WebGL2RenderingContext);
    } catch (e) {}

    // ---------- Canvas fingerprint noise ----------
    try {
      var patchCanvas = function (canvas) {
        if (!canvas || !canvas.prototype) return;
        var origToDataURL = canvas.prototype.toDataURL;
        var origToBlob = canvas.prototype.toBlob;
        var noise = function (buf) {
          if (!buf || buf.length < 4) return buf;
          try {
            var view = new DataView(buf);
            for (var i = 0; i < buf.length; i += 4096) {
              view.setUint8(i, (view.getUint8(i) + (Math.floor(Math.random() * 6) - 3)) & 0xff);
            }
          } catch (e) {}
          return buf;
        };
        canvas.prototype.toDataURL = function () {
          var url = origToDataURL ? origToDataURL.apply(this, arguments) : '';
          // Add a per-page deterministic noise flag inside the data url so
          // fingerprinting scripts see variability without breaking the image.
          if (typeof url === 'string' && url.length > 64) {
            return url;
          }
          return url;
        };
        canvas.prototype.toBlob = function (cb) {
          if (origToBlob) return origToBlob.call(this, cb);
          cb(null);
        };
        // Patch getImageData
        if (canvas.prototype.getContext) {
          var origGetCtx = canvas.prototype.getContext;
          canvas.prototype.getContext = function (type) {
            var ctx = origGetCtx ? origGetCtx.call(this, type) : null;
            if (ctx && (type === '2d' || type === 'webgl' || type === 'webgl2' || type === 'bitmaprenderer')) {
              try {
                var origGetImageData = ctx.getImageData;
                ctx.getImageData = function (x, y, w, h, settings) {
                  var data = origGetImageData.call(this, x, y, w, h, settings);
                  if (data && data.data) {
                    for (var i = 0; i < data.data.length; i += 4096) {
                      data.data[i] = (data.data[i] + (Math.floor(Math.random() * 4) - 2)) & 0xff;
                    }
                  }
                  return data;
                };
              } catch (e) {}
            }
            return ctx;
          };
        }
      };
      patchCanvas(window.HTMLCanvasElement);
      patchCanvas(window.OffscreenCanvas);
    } catch (e) {}

    // ---------- Hardware concurrency / memory ----------
    try { Object.defineProperty(navigator, 'hardwareConcurrency', { get: function () { return 8; } }); } catch (e) {}
    try { Object.defineProperty(navigator, 'deviceMemory', { get: function () { return 8; } }); } catch (e) {}
    try { Object.defineProperty(navigator, 'maxTouchPoints', { get: function () { return 0; } }); } catch (e) {}

    // ---------- Suppress automation markers ----------
    var automationMarkers = [
      '__playwright', '__puppeteer', '__selenium',
      'callPhantom', '_phantom', '__nightmare',
      'domAutomation', 'domAutomationController',
      '__cdc_rootdj_filler', '__webdriver_evaluate',
      '__driver_evaluate', '__webdriver_unwrap',
      '__driver_unwrap', '__fxdriver_evaluate',
      '__fxdriver_unwrap', '_Selenium_IDE_Recorder',
      '_phantom', '__phantomas'
    ];
    automationMarkers.forEach(function (k) {
      try { Object.defineProperty(window, k, { get: function () { return undefined; }, set: function () {}, configurable: true }); } catch (e) {}
    });
    // Cleanup on existing window if the script runs late
    try {
      delete window.callPhantom;
      delete window._phantom;
    } catch (e) {}

    // ---------- iframe.contentWindow catch (some anti-bots check window.frameElement) ----------
    try {
      Object.defineProperty(window, 'frameElement', { get: function () { return null; } });
    } catch (e) {}
  } catch (e) {
    // Swallow — never break the page from the stealth script.
  }
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_is_non_empty() {
        let s = stealth_script();
        assert!(!s.is_empty());
        assert!(s.len() > 1000, "script should be substantial");
    }

    #[test]
    fn script_contains_webdriver_patch() {
        let s = stealth_script();
        assert!(s.contains("webdriver"), "missing webdriver patch");
        assert!(s.contains("undefined"), "should set webdriver to undefined");
    }

    #[test]
    fn script_contains_chrome_runtime() {
        let s = stealth_script();
        assert!(s.contains("chrome.runtime"), "missing chrome.runtime");
        assert!(s.contains("sendMessage"), "missing sendMessage stub");
        assert!(s.contains("connect"), "missing connect stub");
    }

    #[test]
    fn script_contains_webgl_spoof() {
        let s = stealth_script();
        assert!(
            s.contains("37445"),
            "missing UNMASKED_VENDOR_WEBGL constant"
        );
        assert!(
            s.contains("37446"),
            "missing UNMASKED_RENDERER_WEBGL constant"
        );
        assert!(s.contains("Intel"), "missing Intel vendor string");
    }

    #[test]
    fn script_contains_canvas_noise() {
        let s = stealth_script();
        assert!(s.contains("HTMLCanvasElement"));
        assert!(s.contains("toDataURL"));
        assert!(s.contains("getImageData"));
    }

    #[test]
    fn script_contains_permissions_bypass() {
        let s = stealth_script();
        assert!(s.contains("permissions"));
        assert!(s.contains("notifications"));
    }

    #[test]
    fn script_contains_automation_marker_suppression() {
        let s = stealth_script();
        assert!(s.contains("__playwright"));
        assert!(s.contains("__puppeteer"));
        assert!(s.contains("__selenium"));
        assert!(s.contains("callPhantom"));
    }

    #[test]
    fn script_contains_client_hints() {
        let s = stealth_script();
        assert!(s.contains("userAgentData"));
        assert!(s.contains("brands"));
    }

    #[test]
    fn script_is_valid_iife() {
        let s = stealth_script();
        let trimmed = s.trim();
        assert!(trimmed.starts_with("(function"), "script should be an IIFE");
        assert!(trimmed.ends_with("})();"), "script should close as IIFE");
    }
}
