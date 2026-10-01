// Copyright Sudoprivacy contributors. SPDX-License-Identifier: MIT
// Browser-side adapter for the documented local extension bridge protocol.
const endpoint = 'ws://127.0.0.1:9522';
const owned = new Set();
const attached = new Map();
let primary = null;
let socket = null;
let creating = null;
let connecting = false;
const ready = chrome.storage.session.get(['suhTabs', 'suhPrimary']).then(state => {
  for (const id of state.suhTabs || []) owned.add(id);
  primary = state.suhPrimary ?? null;
});
const save = () => chrome.storage.session.set({suhTabs: [...owned], suhPrimary: primary});
const transmit = value => {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(value));
};
async function forget(id) {
  owned.delete(id);
  attached.delete(id);
  if (primary === id) primary = null;
  await save();
}
async function attach(id) {
  if (!owned.has(id)) throw new Error('Tab is not owned by this automation session');
  if (!attached.has(id)) {
    const pending = chrome.debugger.attach({tabId: id}, '1.3').then(async () => {
      await chrome.action.setBadgeText({tabId: id, text: 'AI'});
      await chrome.action.setBadgeBackgroundColor({tabId: id, color: '#5b49e6'});
    }).catch(error => { attached.delete(id); throw error; });
    attached.set(id, pending);
  }
  await attached.get(id);
}
async function create(url) {
  const tab = await chrome.tabs.create({url: url || 'about:blank', active: false});
  owned.add(tab.id);
  if (primary == null) primary = tab.id;
  await save();
  await attach(tab.id);
  return tab;
}
async function targets() {
  await ready;
  const result = [];
  for (const id of [...owned]) {
    try {
      const tab = await chrome.tabs.get(id);
      // Chrome can expose the requested URL before the first navigation commits.
      const url = tab.url || tab.pendingUrl || '';
      result.push({targetId: String(id), type: 'page', title: tab.title || '', url, attached: attached.has(id), canAccessOpener: false});
    } catch { await forget(id); }
  }
  if (!result.length) {
    if (!creating) creating = create('about:blank').finally(() => { creating = null; });
    await creating;
    return targets();
  }
  return result;
}
function checked(id) {
  const value = Number(id);
  if (!owned.has(value)) throw new Error('Tab is not owned by this automation session');
  return value;
}
function cookie(value) {
  const result = {
    name: value.name, value: value.value, domain: value.domain, path: value.path,
    expires: value.expirationDate ?? -1, size: value.name.length + value.value.length,
    httpOnly: value.httpOnly, secure: value.secure, session: value.session,
    priority: 'Medium', sameParty: false, sourceScheme: value.secure ? 'Secure' : 'NonSecure', sourcePort: -1
  };
  const sameSite = {no_restriction: 'None', lax: 'Lax', strict: 'Strict'}[value.sameSite];
  if (sameSite) result.sameSite = sameSite;
  return result;
}
async function command(message) {
  await ready;
  const {method, tab, sessionId} = message;
  const params = message.params || {};
  switch (method) {
    case 'Target.setDiscoverTargets':
    case 'Target.setAutoAttach': return {};
    case 'Target.getTargets': return {targetInfos: await targets()};
    case 'Target.getTargetInfo': {
      const info = (await targets()).find(item => item.targetId === String(params.targetId ?? tab));
      if (!info) throw new Error('No owned target with that id');
      return {targetInfo: info};
    }
    case 'Target.createTarget': return {targetId: String((await create(params.url)).id)};
    case 'Target.activateTarget': {
      const current = await chrome.tabs.update(checked(params.targetId ?? tab), {active: true});
      await chrome.windows.update(current.windowId, {focused: true});
      return {};
    }
    case 'Target.closeTarget': {
      const id = checked(params.targetId ?? tab);
      await chrome.tabs.remove(id);
      await forget(id);
      return {success: true};
    }
    case 'AiDevBrowser.debugState': return {mainTabId: primary, autoTabs: [...owned], attached: [...attached.keys()]};
    case 'Storage.getCookies':
    case 'Network.getAllCookies': return {cookies: (await chrome.cookies.getAll({})).map(cookie)};
    case 'Browser.getWindowForTarget': {
      const current = await chrome.tabs.get(checked(params.targetId ?? tab));
      const window = await chrome.windows.get(current.windowId);
      return {windowId: window.id, bounds: {left: window.left, top: window.top, width: window.width, height: window.height, windowState: window.state}};
    }
    case 'Browser.close': throw new Error('Use browser_disconnect to detach; the user browser cannot be closed through the extension');
    default: {
      const id = checked(tab);
      await attach(id);
      // Input dispatch needs a rendered tab; inactive tabs can withhold its ACK.
      if (method.startsWith('Input.')) await chrome.tabs.update(id, {active: true});
      const destination = sessionId ? {tabId: id, sessionId} : {tabId: id};
      return chrome.debugger.sendCommand(destination, method, params);
    }
  }
}
async function adopt(id, opener) {
  await ready;
  if (!owned.has(opener) || owned.has(id)) return;
  owned.add(id);
  await save();
  try { await attach(id); } catch { /* Retry when the new document is ready. */ }
}
chrome.tabs.onCreated.addListener(tab => { void adopt(tab.id, tab.openerTabId); });
chrome.webNavigation.onCreatedNavigationTarget.addListener(event => { void adopt(event.tabId, event.sourceTabId); });
chrome.tabs.onRemoved.addListener(id => { if (owned.has(id)) void forget(id); });
chrome.debugger.onDetach.addListener(source => { attached.delete(source.tabId); });
chrome.debugger.onEvent.addListener((source, method, params) => {
  if (owned.has(source.tabId)) transmit({_event_tab: String(source.tabId), method, params, ...(source.sessionId ? {sessionId: source.sessionId} : {})});
});
async function connect() {
  if (connecting || socket?.readyState === WebSocket.OPEN) return;
  connecting = true;
  const current = new WebSocket(endpoint);
  socket = current;
  current.onopen = async () => {
    connecting = false;
    let account = null;
    try { account = (await chrome.identity.getProfileUserInfo({accountStatus: 'ANY'})).email || null; } catch { /* Signed-out profile. */ }
    if (socket === current) transmit({_hello: true, account});
  };
  current.onmessage = async event => {
    let message;
    try { message = JSON.parse(event.data); } catch { return; }
    try {
      const result = await command(message);
      if (current.readyState === WebSocket.OPEN) current.send(JSON.stringify({_gid: message._gid, result: result || {}}));
    } catch (error) {
      if (current.readyState === WebSocket.OPEN) current.send(JSON.stringify({_gid: message._gid, error: {message: String(error.message || error)}}));
    }
  };
  current.onerror = () => current.close();
  current.onclose = async () => {
    if (socket !== current) return;
    socket = null;
    connecting = false;
    for (const id of [...attached.keys()]) {
      try { await chrome.debugger.detach({tabId: id}); } catch { /* Tab was closed. */ }
      attached.delete(id);
      try { await chrome.action.setBadgeText({tabId: id, text: ''}); } catch { /* Tab was closed. */ }
    }
    setTimeout(connect, 1000);
  };
}
chrome.alarms.create('suh-reconnect', {periodInMinutes: 0.5});
chrome.alarms.onAlarm.addListener(alarm => { if (alarm.name === 'suh-reconnect') void connect(); });
// Keep an active automation session alive through human login/consent delays.
setInterval(() => { if (owned.size && socket?.readyState === WebSocket.OPEN) void chrome.runtime.getPlatformInfo(); }, 20000);
void connect();
