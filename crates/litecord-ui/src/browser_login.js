// Runs exclusively in Litecord's new sign-in view. Observe a credential used
// by this page after the owner signs in; never inspect stored browser data,
// passwords, login responses, or challenge responses.
(() => {
  if (self !== top || location.origin !== "https://discord.com") return;
  const capability = "__LOGIN_CAPABILITY__";
  const deadline = Date.now() + 600000;
  let sent = false;
  const api = input => {
    try {
      const address = new URL(input, location.href);
      return address.origin === "https://discord.com" && /^\/api\/v[0-9]+\//.test(address.pathname);
    } catch { return false; }
  };
  const handoff = value => {
    if (sent || Date.now() >= deadline || typeof value !== "string" ||
        !/^[\x21-\x7e]{16,2048}$/.test(value)) return;
    sent = true;
    window.ipc.postMessage(capability + value);
  };
  const routes = new WeakMap();
  const nativeOpen = XMLHttpRequest.prototype.open;
  const nativeHeader = XMLHttpRequest.prototype.setRequestHeader;
  XMLHttpRequest.prototype.open = function(method, address, ...args) {
    routes.set(this, api(address));
    return nativeOpen.call(this, method, address, ...args);
  };
  XMLHttpRequest.prototype.setRequestHeader = function(name, value) {
    if (routes.get(this) && String(name).toLowerCase() === "authorization") handoff(value);
    return nativeHeader.call(this, name, value);
  };
  const nativeFetch = window.fetch;
  window.fetch = function(resource, options) {
    if (api(resource instanceof Request ? resource.url : resource)) {
      const headers = new Headers(options?.headers ?? (resource instanceof Request ? resource.headers : undefined));
      handoff(headers.get("authorization"));
    }
    return nativeFetch.apply(this, arguments);
  };
})();
