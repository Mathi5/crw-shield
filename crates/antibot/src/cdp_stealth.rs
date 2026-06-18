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
    // ---------- toString spoofing helper ----------
    // Cloudflare and friends check Function.prototype.toString.call(patchedFn)
    // to detect tampering. We wrap each override so its toString() reports
    // 'function () { [native code] }'.
    var nativeToString = function () { return 'function () { [native code] }'; };
    var makeNative = function (fn, name) {
      try {
        Object.defineProperty(fn, 'name', { value: name || '', configurable: true });
      } catch (e) {}
      try {
        Object.defineProperty(fn, 'toString', {
          value: nativeToString,
          configurable: true,
          writable: true
        });
      } catch (e) {
        try { fn.toString = nativeToString; } catch (e2) {}
      }
      return fn;
    };

    // ---------- navigator.webdriver ----------
    var webdriverGetter = makeNative(function webdriver() { return undefined; }, 'get webdriver');
    Object.defineProperty(Navigator.prototype, 'webdriver', {
      get: webdriverGetter,
      set: function () {},
      configurable: true,
      enumerable: true
    });
    try {
      Object.defineProperty(navigator, 'webdriver', {
        get: webdriverGetter,
        configurable: true
      });
    } catch (e) {}

    // ---------- navigator.languages ----------
    try {
      var languagesGetter = makeNative(function languages() { return ['en-US', 'en']; }, 'get languages');
      Object.defineProperty(navigator, 'languages', {
        get: languagesGetter,
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
      pluginArray.item = makeNative(function item(i) { return this[i] || null; }, 'item');
      pluginArray.namedItem = makeNative(function namedItem(n) { for (var i=0;i<this.length;i++) if (this[i].name===n) return this[i]; return null; }, 'namedItem');
      pluginArray.refresh = makeNative(function refresh() {}, 'refresh');
      var pluginsGetter = makeNative(function plugins() { return pluginArray; }, 'get plugins');
      Object.defineProperty(navigator, 'plugins', {
        get: pluginsGetter,
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
        getHighEntropyValues: makeNative(function getHighEntropyValues(hints) {
          var out = {};
          if (hints.indexOf('architecture') !== -1) out.architecture = 'x86';
          if (hints.indexOf('bitness') !== -1) out.bitness = '64';
          if (hints.indexOf('model') !== -1) out.model = '';
          if (hints.indexOf('platform') !== -1) out.platform = 'Linux';
          if (hints.indexOf('platformVersion') !== -1) out.platformVersion = '6.1.0';
          if (hints.indexOf('uaFullVersion') !== -1) out.uaFullVersion = '131.0.6778.85';
          return Promise.resolve(out);
        }, 'getHighEntropyValues'),
        toJSON: makeNative(function toJSON() { return { brands: brands, mobile: false, platform: 'Linux x86_64' }; }, 'toJSON')
      };
      var uaDataGetter = makeNative(function userAgentData() { return uaData; }, 'get userAgentData');
      Object.defineProperty(navigator, 'userAgentData', {
        get: uaDataGetter,
        configurable: true
      });
    } catch (e) {}

    // ---------- navigator.userAgent / appVersion (JS-level fallback) ----------
    // NOTE: the authoritative User-Agent string should be set via CDP
    // Network.setUserAgentOverride — a JS-level navigator.userAgent override
    // is itself a fingerprinting signal. This fallback exists only for the
    // case where CDP override is unavailable; it is kept consistent with
    // appVersion and the client hints above.
    try {
      var UA_STR = 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36';
      var APP_VERSION_STR = '5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36';
      var uaGetter = makeNative(function userAgent() { return UA_STR; }, 'get userAgent');
      var appVersionGetter = makeNative(function appVersion() { return APP_VERSION_STR; }, 'get appVersion');
      Object.defineProperty(Navigator.prototype, 'userAgent', {
        get: uaGetter,
        configurable: true
      });
      Object.defineProperty(Navigator.prototype, 'appVersion', {
        get: appVersionGetter,
        configurable: true
      });
      try {
        Object.defineProperty(navigator, 'userAgent', { get: uaGetter, configurable: true });
      } catch (e) {}
      try {
        Object.defineProperty(navigator, 'appVersion', { get: appVersionGetter, configurable: true });
      } catch (e) {}
    } catch (e) {}

    // ---------- navigator.connection (Network Information API) ----------
    try {
      var connection = {
        effectiveType: '4g',
        rtt: 50,
        downlink: 10,
        saveData: false,
        type: 'wifi',
        onchange: null,
        ontypechange: null,
        addEventListener: makeNative(function addEventListener() {}, 'addEventListener'),
        removeEventListener: makeNative(function removeEventListener() {}, 'removeEventListener'),
        dispatchEvent: makeNative(function dispatchEvent() { return true; }, 'dispatchEvent')
      };
      var connectionGetter = makeNative(function connection() { return connection; }, 'get connection');
      Object.defineProperty(Navigator.prototype, 'connection', {
        get: connectionGetter,
        configurable: true
      });
      try {
        Object.defineProperty(navigator, 'connection', { get: connectionGetter, configurable: true });
      } catch (e) {}
    } catch (e) {}

    // ---------- window.chrome ----------
    try {
      if (!window.chrome) window.chrome = {};
      window.chrome.runtime = {
        PlatformOs: { MAC: 'mac', WIN: 'win', ANDROID: 'android', CROS: 'cros', LINUX: 'linux', OPENBSD: 'openbsd' },
        PlatformArch: { ARM: 'arm', X86_32: 'x86-32', X86_64: 'x86-64' },
        RequestUpdateCheckStatus: { THROTTLED: 'throttled', NO_UPDATE: 'no_update', UPDATE_AVAILABLE: 'update_available' },
        OnInstalledReason: { CHROME_UPDATE: 'chrome_update', ON_UPDATE_URL_fetch: 'ondemand', SHARED_MODULE_UPDATE: 'shared_module_update' },
        OnRestartRequiredReason: { APP_UPDATE: 'app_update', OS_UPDATE: 'os_update', PERIODIC: 'periodic' },
        connect: makeNative(function connect() { return { onMessage: { addListener: function () {} }, postMessage: function () {}, onDisconnect: { addListener: function () {} } }; }, 'connect'),
        sendMessage: makeNative(function sendMessage(_msg, _opts, cb) { if (typeof cb === 'function') { try { cb({}); } catch (e) {} } return Promise.resolve({}); }, 'sendMessage')
      };

      // ---------- chrome.webstore (some detection probes for it) ----------
      window.chrome.webstore = window.chrome.webstore || {};
      try {
        Object.defineProperty(window.chrome, 'webstore', {
          value: {
            InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' },
            onInstallStageChanged: {},
            onDownloadProgress: {},
            install: makeNative(function install() {}, 'install'),
            getBrowserLogin: makeNative(function getBrowserLogin() {}, 'getBrowserLogin'),
            getLoginStatus: makeNative(function getLoginStatus() {}, 'getLoginStatus'),
            getStoreStatus: makeNative(function getStoreStatus(cb) { if (typeof cb === 'function') { try { cb('installed'); } catch (e) {} } }, 'getStoreStatus')
          },
          configurable: true,
          writable: true
        });
      } catch (e) {
        try { window.chrome.webstore = window.chrome.webstore || {}; } catch (e2) {}
      }
    } catch (e) {}

    // ---------- chrome.csi / chrome.loadTimes ----------
    try {
      var now = function () { return Date.now() / 1000; };
      var start = now();
      window.chrome.csi = makeNative(function csi() {
        return { startE: start, onloadT: now() - start, pageT: now() - start, tran: 15 };
      }, 'csi');
      window.chrome.loadTimes = makeNative(function loadTimes() {
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
      }, 'loadTimes');
    } catch (e) {}

    // ---------- chrome.app ----------
    try {
      if (!window.chrome) window.chrome = {};
      window.chrome.app = {
        isInstalled: false,
        InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' },
        RunningState: { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' },
        getDetails: makeNative(function getDetails() { return null; }, 'getDetails'),
        getIsInstalled: makeNative(function getIsInstalled() { return false; }, 'getIsInstalled'),
        installState: makeNative(function installState(cb) { if (typeof cb === 'function') cb('not_installed'); }, 'installState')
      };
    } catch (e) {}

    // ---------- Permissions API bypass ----------
    // Notification.permission is overridden to "default" below; permissions.query
    // returns the same value so the two paths are consistent.
    try {
      if (navigator.permissions && navigator.permissions.query) {
        var originalQuery = navigator.permissions.query.bind(navigator.permissions);
        var wrappedQuery = makeNative(function query(descriptor) {
          if (descriptor && descriptor.name === 'notifications') {
            try { return Promise.resolve({ state: (typeof Notification !== 'undefined' ? Notification.permission : 'default') || 'default', onchange: null }); }
            catch (e) { return Promise.resolve({ state: 'default', onchange: null }); }
          }
          try { return originalQuery(descriptor); }
          catch (e) { return Promise.reject(e); }
        }, 'query');
        try { navigator.permissions.query = wrappedQuery; } catch (e) {}
      }
      if (typeof window.Notification === 'function') {
        try { Object.defineProperty(Notification, 'permission', { get: makeNative(function permission() { return 'default'; }, 'get permission'), configurable: true }); } catch (e) {}
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
        var wrapped = makeNative(function getParameter(param) {
          if (param === WEBGL_VENDOR) return VENDOR_STR;
          if (param === WEBGL_RENDERER) return RENDERER_STR;
          return orig.call(this, param);
        }, 'getParameter');
        try { proto.prototype.getParameter = wrapped; } catch (e) {}
      };
      patchGetParameter(window.WebGLRenderingContext);
      patchGetParameter(window.WebGL2RenderingContext);

      var patchDebug = function (proto) {
        if (!proto || !proto.prototype) return;
        if (proto.prototype.getVendor) proto.prototype.getVendor = makeNative(function getVendor() { return VENDOR_STR; }, 'getVendor');
        if (proto.prototype.getRenderer) proto.prototype.getRenderer = makeNative(function getRenderer() { return RENDERER_STR; }, 'getRenderer');
        if (proto.prototype.getUnmaskedVendorWebgl) proto.prototype.getUnmaskedVendorWebgl = makeNative(function getUnmaskedVendorWebgl() { return VENDOR_STR; }, 'getUnmaskedVendorWebgl');
        if (proto.prototype.getUnmaskedRendererWebgl) proto.prototype.getUnmaskedRendererWebgl = makeNative(function getUnmaskedRendererWebgl() { return RENDERER_STR; }, 'getUnmaskedRendererWebgl');
        if (proto.prototype.getExtension) {
          var origExt = proto.prototype.getExtension;
          proto.prototype.getExtension = makeNative(function getExtension(name) {
            var ext = origExt.call(this, name);
            if (ext && name && /WEBGL_debug/i.test(name)) {
              try { ext.UNMASKED_VENDOR_WEBGL = WEBGL_VENDOR; ext.UNMASKED_RENDERER_WEBGL = WEBGL_RENDERER; } catch (e) {}
            }
            return ext;
          }, 'getExtension');
        }
      };
      patchDebug(window.WebGLRenderingContext);
      patchDebug(window.WebGL2RenderingContext);
    } catch (e) {}

    // ---------- Canvas fingerprint noise ----------
    try {
      var patchCanvas = function (canvas) {
        if (!canvas || !canvas.prototype) return;

        // Patch getContext so we can hook toDataURL / getImageData on the
        // returned 2d instance. Doing it at instance level (instead of on
        // CanvasRenderingContext2D.prototype) avoids breaking unrelated
        // contexts that share the prototype.
        var origGetCtx = canvas.prototype.getContext;
        canvas.prototype.getContext = makeNative(function getContext(type) {
          var ctx = origGetCtx ? origGetCtx.apply(this, arguments) : null;
          if (ctx && (type === '2d') && !ctx.__stealthPatched) {
            try {
              var origGetImageData = ctx.getImageData;
              ctx.getImageData = makeNative(function getImageData(x, y, w, h, settings) {
                var data = origGetImageData.call(this, x, y, w, h, settings);
                if (data && data.data) {
                  for (var i = 0; i < data.data.length; i += 4096) {
                    data.data[i] = (data.data[i] + (Math.floor(Math.random() * 4) - 2)) & 0xff;
                  }
                }
                return data;
              }, 'getImageData');

              // toDataURL noise: render the source canvas to an offscreen
              // canvas, perturb a handful of pixels via getImageData /
              // putImageData, then return its toDataURL.
              var origToDataURL = ctx.canvas ? null : null;
              var canvasEl = this;
              ctx.toDataURL = makeNative(function toDataURL() {
                try {
                  var w2 = canvasEl.width || 1;
                  var h2 = canvasEl.height || 1;
                  var off = document.createElement('canvas');
                  off.width = w2;
                  off.height = h2;
                  var offCtx = off.getContext('2d');
                  if (offCtx) {
                    offCtx.drawImage(canvasEl, 0, 0);
                    var img = offCtx.getImageData(0, 0, w2, h2);
                    if (img && img.data) {
                      // Touch the first 256 pixels (R,G,B of the first 85 px or
                      // top-left block) — enough to perturb the hash while
                      // remaining visually invisible.
                      var n = Math.min(256 * 4, img.data.length);
                      for (var i2 = 0; i2 < n; i2 += 4) {
                        img.data[i2]     = (img.data[i2]     + (Math.floor(Math.random() * 4) - 2)) & 0xff;
                        img.data[i2 + 1] = (img.data[i2 + 1] + (Math.floor(Math.random() * 4) - 2)) & 0xff;
                        img.data[i2 + 2] = (img.data[i2 + 2] + (Math.floor(Math.random() * 4) - 2)) & 0xff;
                      }
                      offCtx.putImageData(img, 0, 0);
                      return off.toDataURL.apply(off, arguments);
                    }
                  }
                } catch (e) {}
                // Fallback: behave like the real toDataURL.
                if (canvasEl && typeof canvasEl.toDataURL === 'function') {
                  return canvasEl.toDataURL.apply(canvasEl, arguments);
                }
                return '';
              }, 'toDataURL');

              try { ctx.__stealthPatched = true; } catch (e) {}
            } catch (e) {}
          }
          return ctx;
        }, 'getContext');

        var origToBlob = canvas.prototype.toBlob;
        canvas.prototype.toBlob = makeNative(function toBlob(cb) {
          if (origToBlob) return origToBlob.call(this, cb);
          cb(null);
        }, 'toBlob');
      };
      patchCanvas(window.HTMLCanvasElement);
      patchCanvas(window.OffscreenCanvas);
    } catch (e) {}

    // ---------- AudioContext fingerprint noise ----------
    // createOscillator + createAnalyser expose a deterministic floating-point
    // fingerprint via getFloatFrequencyData / getByteFrequencyData. Add a
    // tiny, deterministic per-call perturbation so the fingerprint varies.
    try {
      var audioSeed = (Date.now() & 0xffff) ^ 0x9e37;
      var perturbFreq = function (arr, channels) {
        if (!arr || typeof arr.length !== 'number') return;
        var stride = Math.max(1, Math.floor(arr.length / 64));
        for (var i = 0; i < arr.length; i += stride) {
          var d = ((i * 1103515245 + audioSeed + channels * 12345) & 0xff) / 255 - 0.5;
          arr[i] = arr[i] + d;
        }
      };
      var patchAudioContext = function (Ctx) {
        if (!Ctx || !Ctx.prototype) return;
        var origCreateOsc = Ctx.prototype.createOscillator;
        if (origCreateOsc) {
          Ctx.prototype.createOscillator = makeNative(function createOscillator() {
            var osc = origCreateOsc.call(this);
            try {
              var origConnect = osc.connect.bind(osc);
              osc.connect = makeNative(function connect() { return origConnect.apply(this, arguments); }, 'connect');
            } catch (e) {}
            return osc;
          }, 'createOscillator');
        }
        var origCreateAna = Ctx.prototype.createAnalyser;
        if (origCreateAna) {
          Ctx.prototype.createAnalyser = makeNative(function createAnalyser() {
            var ana = origCreateAna.call(this);
            try {
              var origGetByte = ana.getByteFrequencyData.bind(ana);
              var origGetFloat = ana.getFloatFrequencyData.bind(ana);
              ana.getByteFrequencyData = makeNative(function getByteFrequencyData(arr) {
                var r = origGetByte(arr);
                perturbFreq(arr, 1);
                return r;
              }, 'getByteFrequencyData');
              ana.getFloatFrequencyData = makeNative(function getFloatFrequencyData(arr) {
                var r = origGetFloat(arr);
                perturbFreq(arr, 2);
                return r;
              }, 'getFloatFrequencyData');
            } catch (e) {}
            return ana;
          }, 'createAnalyser');
        }
      };
      patchAudioContext(window.AudioContext);
      patchAudioContext(window.OfflineAudioContext);
      patchAudioContext(window.webkitAudioContext);
    } catch (e) {}

    // ---------- Screen properties consistency ----------
    // Force a coherent desktop screen: 1280x800 viewport, 1280x720 available
    // (taskbar takes 80px), 24-bit color, landscape primary orientation.
    try {
      var screenProps = {
        width: 1280,
        height: 800,
        availWidth: 1280,
        availHeight: 720,
        colorDepth: 24,
        pixelDepth: 24,
        orientation: { type: 'landscape-primary', angle: 0 }
      };
      Object.keys(screenProps).forEach(function (k) {
        try {
          Object.defineProperty(window.screen, k, {
            get: makeNative(function () { return screenProps[k]; }, 'get ' + k),
            configurable: true
          });
        } catch (e) {}
      });
    } catch (e) {}

    // ---------- Hardware concurrency / memory ----------
    try {
      Object.defineProperty(navigator, 'hardwareConcurrency', {
        get: makeNative(function () { return 8; }, 'get hardwareConcurrency'),
        configurable: true
      });
    } catch (e) {}
    try {
      Object.defineProperty(navigator, 'deviceMemory', {
        get: makeNative(function () { return 8; }, 'get deviceMemory'),
        configurable: true
      });
    } catch (e) {}
    try {
      Object.defineProperty(navigator, 'maxTouchPoints', {
        get: makeNative(function () { return 0; }, 'get maxTouchPoints'),
        configurable: true
      });
    } catch (e) {}

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
      try {
        Object.defineProperty(window, k, {
          get: makeNative(function () { return undefined; }, 'get ' + k),
          set: function () {},
          configurable: true
        });
      } catch (e) {}
    });
    try {
      delete window.callPhantom;
      delete window._phantom;
    } catch (e) {}

    // ---------- iframe.contentWindow catch (some anti-bots check window.frameElement) ----------
    try {
      Object.defineProperty(window, 'frameElement', {
        get: makeNative(function () { return null; }, 'get frameElement'),
        configurable: true
      });
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
