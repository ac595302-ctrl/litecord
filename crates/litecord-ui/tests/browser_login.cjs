// Offline tests: the page, network methods, and credentials are all synthetic.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const script = fs.readFileSync(path.join(__dirname, '../src/browser_login.js'), 'utf8');

function fixture({ origin = 'https://discord.com', child = false } = {}) {
  const delivered = [];
  let now = 0;
  let requests = 0;
  class Xhr {
    open() { requests++; }
    setRequestHeader() {}
  }
  class Request { constructor(url, headers) { this.url = url; this.headers = headers; } }
  class Headers {
    constructor(values = {}) { this.values = Object.fromEntries(Object.entries(values).map(([k, v]) => [k.toLowerCase(), v])); }
    get(name) { return this.values[name.toLowerCase()] ?? null; }
  }
  const window = { fetch() { requests++; return 'request-forwarded'; }, ipc: { postMessage(body) { delivered.push(body); } } };
  const context = vm.createContext({ window, self: window, top: child ? {} : window,
    location: { origin, href: origin + '/login' }, URL, Request, Headers,
    XMLHttpRequest: Xhr, Date: { now: () => now } });
  vm.runInContext(script.replace('__LOGIN_CAPABILITY__', 'synthetic-cap:'), context);
  return { window, Xhr, Request, delivered, expire: () => { now = 600001; }, requests: () => requests };
}

const secret = 'SYNTHETIC_CREDENTIAL_0123456789';
{
  const f = fixture();
  assert.equal(f.window.fetch('/api/v9/users/@me', { headers: { Authorization: secret } }), 'request-forwarded');
  f.window.fetch('/api/v9/users/@me', { headers: { Authorization: secret } });
  assert.deepEqual(f.delivered, ['synthetic-cap:' + secret]);
  assert.equal(f.requests(), 2);
}
{
  const f = fixture();
  f.window.fetch('https://example.com/api/v9/users/@me', { headers: { authorization: secret } });
  f.window.fetch('/login', { headers: { authorization: secret } });
  f.window.fetch('/api/v9/users/@me', { headers: { authorization: 'Bearer ' + secret } });
  f.window.fetch('/api/v9/users/@me', { headers: { authorization: 'x'.repeat(2049) } });
  f.expire();
  f.window.fetch('/api/v9/users/@me', { headers: { authorization: secret } });
  assert.equal(f.delivered.length, 0);
  assert.equal(f.requests(), 5);
}
for (const options of [{ origin: 'https://discord.com.evil.test' }, { child: true }]) {
  const f = fixture(options);
  f.window.fetch('/api/v9/users/@me', { headers: { authorization: secret } });
  assert.equal(f.delivered.length, 0);
}
{
  const f = fixture();
  const request = new f.Xhr();
  request.open('GET', '/api/v9/users/@me');
  request.setRequestHeader('authorization', secret);
  request.setRequestHeader('authorization', secret);
  assert.deepEqual(f.delivered, ['synthetic-cap:' + secret]);
}
{
  const f = fixture();
  f.window.fetch(new f.Request('https://discord.com/api/v9/users/@me', { Authorization: secret }));
  assert.equal(f.delivered.length, 1);
}
console.log('Browser login handoff: 6 offline scenarios passed. No network requests or real credentials used.');
