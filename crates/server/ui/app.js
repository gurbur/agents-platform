'use strict';

// Read-only owner console for agents-platform (slice S6).
//
// Every request is a GET to the v1 API. The owner token lives in memory and,
// only when the user opts in, in sessionStorage; never in persistent storage,
// cookies or the URL. Server data reaches the DOM through textContent only.

const TOKEN_STORAGE_KEY = 'agents-platform.owner-token';
const NULL_TEXT = '—';
const DELIVERY_STATES = ['idle', 'no_receiver', 'pending', 'blocked'];

const ENDPOINTS = Object.freeze({
  agents: () => '/v1/agents',
  history: () => '/v1/messages',
  agentInbox: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/inbox`,
  agentDelivery: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/delivery`,
});

// The single piece of mutable state. `session` changes on connect and logout
// so a response that arrives afterwards is dropped instead of rendered.
const state = {
  token: '',
  session: 0,
  activeTab: 'directory',
  loaded: { directory: false, history: false, inbox: false, delivery: false },
  agents: [],
  history: { roomId: 'all', q: '', items: [], nextBefore: null, hasMore: false, busy: false },
  inbox: {
    agentId: '',
    agentIdShown: '',
    readMarker: null,
    untilSeq: null,
    nextAfter: null,
    items: [],
    hasMore: false,
    busy: false,
  },
  delivery: { rows: [], busy: false },
};

// ---------------------------------------------------------------- DOM helpers

function byId(id) {
  return document.getElementById(id);
}

function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) {
    node.textContent = text;
  }
  if (className) {
    node.className = className;
  }
  return node;
}

function displayValue(value) {
  if (value === null || value === undefined) {
    return NULL_TEXT;
  }
  return String(value);
}

// Fills `tbody` with one row per item; `columns` are accessors returning the
// cell value, or `{ value, className }` for a styled cell.
function renderRows(tbody, items, columns) {
  tbody.replaceChildren();
  if (items.length === 0) {
    const cell = element('td', '항목 없음', 'empty');
    cell.colSpan = columns.length;
    const row = element('tr');
    row.append(cell);
    tbody.append(row);
    return;
  }
  for (const item of items) {
    const row = element('tr');
    for (const column of columns) {
      const cell = column(item);
      const isStyled = cell !== null && typeof cell === 'object';
      const value = isStyled ? cell.value : cell;
      row.append(element('td', displayValue(value), isStyled ? cell.className : ''));
    }
    tbody.append(row);
  }
}

function replaceOptions(select, values, labelOf) {
  const previous = select.value;
  select.replaceChildren();
  for (const value of values) {
    const option = element('option', labelOf(value));
    option.value = value;
    select.append(option);
  }
  if (values.includes(previous)) {
    select.value = previous;
  }
}

// ------------------------------------------------------------------- errors

class ApiFailure extends Error {
  constructor(status, code, message, requestId) {
    super(message);
    this.status = status;
    this.code = code;
    this.requestId = requestId;
  }
}

function errorBox(panel) {
  return byId(`${panel}-error`);
}

function showError(panel, failure, suffix) {
  const box = errorBox(panel);
  const status = failure.status ? `${failure.status} ` : '';
  const text = suffix ? `${failure.message} ${suffix}` : failure.message;
  box.replaceChildren(
    element('span', `${status}${failure.code}`, 'code'),
    document.createTextNode(` — ${text}`),
    element('span', `request_id: ${failure.requestId || NULL_TEXT}`, 'request-id'),
  );
  box.hidden = false;
}

function clearError(panel) {
  const box = errorBox(panel);
  box.replaceChildren();
  box.hidden = true;
}

function asFailure(error) {
  if (error instanceof ApiFailure) {
    return error;
  }
  return new ApiFailure(0, 'client_error', String(error), '');
}

// ---------------------------------------------------------------------- API

function buildUrl(path, params) {
  const url = new URL(path, window.location.origin);
  for (const [name, value] of Object.entries(params || {})) {
    if (value !== null && value !== undefined && value !== '') {
      url.searchParams.set(name, value);
    }
  }
  return url;
}

async function readJson(response) {
  const text = await response.text();
  if (text === '') {
    return null;
  }
  try {
    return JSON.parse(text);
  } catch (_) {
    return null;
  }
}

function failureFrom(response, body) {
  const error = body && body.error;
  if (error && typeof error.code === 'string') {
    return new ApiFailure(response.status, error.code, String(error.message), String(error.request_id || ''));
  }
  const requestId = response.headers.get('x-request-id') || '';
  return new ApiFailure(response.status, 'unexpected_response', response.statusText || 'no error body', requestId);
}

// The only way this page talks to the server. Without a token the request
// goes out unauthenticated, so the server's 401 is what the user sees.
async function apiGet(path, params) {
  const headers = {};
  if (state.token) {
    headers.Authorization = `Bearer ${state.token}`;
  }
  let response;
  try {
    response = await fetch(buildUrl(path, params), {
      method: 'GET',
      headers,
      cache: 'no-store',
      credentials: 'omit',
      redirect: 'error',
    });
  } catch (error) {
    throw new ApiFailure(0, 'network_error', String(error.message || error), '');
  }
  const body = await readJson(response);
  if (!response.ok) {
    throw failureFrom(response, body);
  }
  return body;
}

// -------------------------------------------------------------------- token

function readStoredToken() {
  try {
    return window.sessionStorage.getItem(TOKEN_STORAGE_KEY) || '';
  } catch (_) {
    return '';
  }
}

function storeToken(token, keep) {
  try {
    if (keep) {
      window.sessionStorage.setItem(TOKEN_STORAGE_KEY, token);
    } else {
      window.sessionStorage.removeItem(TOKEN_STORAGE_KEY);
    }
  } catch (_) {
    // Storage may be unavailable (private mode); the token stays in memory.
  }
}

function setStatus(text, kind) {
  const status = byId('connection-status');
  status.textContent = text;
  status.className = kind ? `status ${kind}` : 'status';
}

function resetData() {
  state.session += 1;
  state.loaded = { directory: false, history: false, inbox: false, delivery: false };
  state.agents = [];
  state.history = { roomId: 'all', q: '', items: [], nextBefore: null, hasMore: false, busy: false };
  state.inbox = {
    agentId: '',
    agentIdShown: '',
    readMarker: null,
    untilSeq: null,
    nextAfter: null,
    items: [],
    hasMore: false,
    busy: false,
  };
  state.delivery = { rows: [], busy: false };
  for (const panel of ['directory', 'history', 'inbox', 'delivery']) {
    clearError(panel);
  }
  renderAll();
}

async function connect(event) {
  event.preventDefault();
  const token = byId('token-input').value.trim();
  const keep = byId('token-save').checked;
  state.token = token;
  storeToken(token, keep);
  byId('token-input').value = '';
  await startSession();
}

async function startSession() {
  resetData();
  setStatus('연결 중…');
  await loadDirectory();
  if (state.activeTab !== 'directory') {
    await loadTab(state.activeTab);
  }
}

function logout() {
  state.token = '';
  storeToken('', false);
  byId('token-input').value = '';
  byId('token-save').checked = false;
  resetData();
  setStatus('로그아웃됨 — token 없음');
}

// --------------------------------------------------------------------- tabs

function activateTab(name) {
  state.activeTab = name;
  for (const tab of document.querySelectorAll('[role="tab"]')) {
    const selected = tab.dataset.tab === name;
    tab.setAttribute('aria-selected', String(selected));
    byId(tab.getAttribute('aria-controls')).hidden = !selected;
  }
  // Without a token there is nothing to show yet; the refresh buttons still
  // send the request so the server's 401 is visible.
  if (state.token && !state.loaded[name]) {
    loadTab(name);
  }
}

async function loadTab(name) {
  const loaders = {
    directory: loadDirectory,
    history: searchHistory,
    inbox: openInbox,
    delivery: loadDelivery,
  };
  await loaders[name]();
}

// ---------------------------------------------------------------- directory

async function loadDirectory() {
  const session = state.session;
  clearError('directory');
  byId('directory-refresh').disabled = true;
  try {
    const body = await apiGet(ENDPOINTS.agents());
    if (session !== state.session) {
      return;
    }
    state.agents = body.items;
    state.loaded.directory = true;
    setStatus(`연결됨 — agent ${state.agents.length}개`, 'ok');
  } catch (error) {
    if (session !== state.session) {
      return;
    }
    const failure = asFailure(error);
    showError('directory', failure);
    setStatus(`연결 실패 — ${failure.code}`, 'bad');
  } finally {
    byId('directory-refresh').disabled = false;
    renderDirectory();
  }
}

function agentIds() {
  return state.agents.map((agent) => agent.agent_id).sort();
}

// Every canonical DM room between two directory agents: `dm:<a>:<b>`, a < b.
function dmRooms(ids) {
  const rooms = [];
  for (let i = 0; i < ids.length; i += 1) {
    for (let j = i + 1; j < ids.length; j += 1) {
      rooms.push(`dm:${ids[i]}:${ids[j]}`);
    }
  }
  return rooms;
}

function deliveryStateCell(value) {
  const className = DELIVERY_STATES.includes(value) ? `state-${value}` : '';
  return { value, className };
}

function renderDirectory() {
  renderRows(byId('directory-rows'), state.agents, [
    (agent) => agent.agent_id,
    (agent) => agent.name,
    (agent) => agent.joined_all,
    (agent) => agent.receiver_url,
    (agent) => agent.route_version,
    (agent) => agent.created_at,
    (agent) => deliveryStateCell(agent.delivery ? agent.delivery.state : null),
    (agent) => (agent.profile ? agent.profile.role : null),
  ]);
  const ids = agentIds();
  replaceOptions(byId('history-room'), ['all', ...dmRooms(ids)], (room) => room);
  replaceOptions(byId('inbox-agent'), ids, (id) => id);
}

// ------------------------------------------------------------------ history

async function searchHistory(event) {
  if (event) {
    event.preventDefault();
  }
  if (state.history.busy) {
    return;
  }
  state.history.roomId = byId('history-room').value || 'all';
  state.history.q = byId('history-query').value;
  state.history.items = [];
  state.history.nextBefore = null;
  state.history.hasMore = false;
  await fetchHistoryPage();
}

async function loadMoreHistory() {
  await fetchHistoryPage();
}

// Newest first; a follow-up page passes the last event_seq as the exclusive
// `before_event_seq`. `limit` is left to the server default.
async function fetchHistoryPage() {
  if (state.history.busy) {
    return;
  }
  const session = state.session;
  const history = state.history;
  history.busy = true;
  clearError('history');
  renderHistory();
  try {
    const body = await apiGet(ENDPOINTS.history(), {
      room_id: history.roomId,
      before_event_seq: history.nextBefore,
      q: history.q,
    });
    if (session !== state.session) {
      return;
    }
    history.items = history.items.concat(body.items);
    history.nextBefore = body.next_before_event_seq;
    history.hasMore = body.has_more;
    state.loaded.history = true;
  } catch (error) {
    if (session === state.session) {
      showError('history', asFailure(error));
    }
  } finally {
    history.busy = false;
    if (session === state.session) {
      renderHistory();
    }
  }
}

function renderHistory() {
  const history = state.history;
  renderRows(byId('history-rows'), history.items, [
    (message) => message.event_seq,
    (message) => message.created_at,
    (message) => message.room_id,
    (message) => message.sender_id,
    (message) => message.reply_to,
    (message) => message.message_id,
    (message) => ({ value: message.body, className: 'body' }),
  ]);
  const search = history.q ? ` · q="${history.q}"` : '';
  byId('history-summary').textContent = state.loaded.history
    ? `${history.roomId}${search} · ${history.items.length}건 표시 · has_more=${history.hasMore}`
    : '';
  const more = byId('history-more');
  more.hidden = !history.hasMore;
  more.disabled = history.busy;
}

// -------------------------------------------------------------------- inbox

async function openInbox(event) {
  if (event) {
    event.preventDefault();
  }
  const agentId = byId('inbox-agent').value;
  if (!agentId || state.inbox.busy) {
    return;
  }
  const inbox = state.inbox;
  inbox.agentId = agentId;
  inbox.readMarker = null;
  inbox.untilSeq = null;
  inbox.nextAfter = byId('inbox-start').value === 'zero' ? '0' : null;
  inbox.items = [];
  inbox.hasMore = false;
  await fetchInboxPage();
}

async function loadMoreInbox() {
  await fetchInboxPage();
}

// The first page fixes `snapshot_head_seq`; later pages send it back as
// `until_seq` with `after_seq` = the previous `next_after_seq`. GET never
// moves the read marker.
async function fetchInboxPage() {
  const inbox = state.inbox;
  if (inbox.busy) {
    return;
  }
  const session = state.session;
  inbox.busy = true;
  clearError('inbox');
  renderInbox();
  try {
    const body = await apiGet(ENDPOINTS.agentInbox(inbox.agentId), {
      after_seq: inbox.nextAfter,
      until_seq: inbox.untilSeq,
    });
    if (session !== state.session) {
      return;
    }
    inbox.agentIdShown = body.agent_id;
    inbox.readMarker = body.read_marker;
    inbox.untilSeq = body.snapshot_head_seq;
    inbox.nextAfter = body.next_after_seq;
    inbox.items = inbox.items.concat(body.items);
    inbox.hasMore = body.has_more;
    state.loaded.inbox = true;
  } catch (error) {
    if (session === state.session) {
      showError('inbox', asFailure(error));
    }
  } finally {
    inbox.busy = false;
    if (session === state.session) {
      renderInbox();
    }
  }
}

function renderFacts(list, facts) {
  list.replaceChildren();
  for (const [name, value] of facts) {
    const pair = element('div');
    pair.append(element('dt', name), element('dd', displayValue(value)));
    list.append(pair);
  }
}

function renderInbox() {
  const inbox = state.inbox;
  renderRows(byId('inbox-rows'), inbox.items, [
    (item) => item.inbox_seq,
    (item) => item.event_seq,
    (item) => item.created_at,
    (item) => item.room_id,
    (item) => item.sender_id,
    (item) => item.message_id,
    (item) => ({ value: item.body, className: 'body' }),
  ]);
  const facts = state.loaded.inbox
    ? [
      ['agent_id', inbox.agentIdShown],
      ['read_marker', inbox.readMarker],
      ['snapshot_head_seq (until_seq)', inbox.untilSeq],
      ['표시', `${inbox.items.length}건`],
      ['has_more', inbox.hasMore],
    ]
    : [];
  renderFacts(byId('inbox-summary'), facts);
  const more = byId('inbox-more');
  more.hidden = !inbox.hasMore;
  more.disabled = inbox.busy;
}

// ----------------------------------------------------------------- delivery

async function loadDelivery() {
  const delivery = state.delivery;
  if (delivery.busy) {
    return;
  }
  const session = state.session;
  const ids = agentIds();
  delivery.busy = true;
  clearError('delivery');
  byId('delivery-refresh').disabled = true;
  try {
    const results = await Promise.allSettled(
      ids.map((agentId) => apiGet(ENDPOINTS.agentDelivery(agentId))),
    );
    if (session !== state.session) {
      return;
    }
    delivery.rows = [];
    const failures = [];
    results.forEach((result, index) => {
      if (result.status === 'fulfilled') {
        delivery.rows.push({ agent_id: ids[index], ...result.value });
      } else {
        failures.push(asFailure(result.reason));
      }
    });
    if (failures.length > 0) {
      showError('delivery', failures[0], `(실패 ${failures.length}/${ids.length} agent)`);
    }
    state.loaded.delivery = true;
  } finally {
    delivery.busy = false;
    byId('delivery-refresh').disabled = false;
    if (session === state.session) {
      renderDelivery();
    }
  }
}

function renderDelivery() {
  renderRows(byId('delivery-rows'), state.delivery.rows, [
    (row) => row.agent_id,
    (row) => deliveryStateCell(row.state),
    (row) => row.read_marker,
    (row) => row.inbox_head_seq,
    (row) => row.pending_seq,
    (row) => row.last_accepted_seq,
    (row) => row.route_version,
    (row) => row.receiver_url_present,
    (row) => row.last_attempt_at,
    (row) => row.last_error,
  ]);
}

// --------------------------------------------------------------------- boot

function renderAll() {
  renderDirectory();
  renderHistory();
  renderInbox();
  renderDelivery();
}

function bindEvents() {
  byId('token-form').addEventListener('submit', connect);
  byId('logout-button').addEventListener('click', logout);
  for (const tab of document.querySelectorAll('[role="tab"]')) {
    tab.addEventListener('click', () => activateTab(tab.dataset.tab));
  }
  byId('directory-refresh').addEventListener('click', loadDirectory);
  byId('history-form').addEventListener('submit', searchHistory);
  byId('history-more').addEventListener('click', loadMoreHistory);
  byId('inbox-form').addEventListener('submit', openInbox);
  byId('inbox-more').addEventListener('click', loadMoreInbox);
  byId('delivery-refresh').addEventListener('click', loadDelivery);
}

function init() {
  bindEvents();
  renderAll();
  const stored = readStoredToken();
  if (!stored) {
    return;
  }
  state.token = stored;
  byId('token-save').checked = true;
  startSession();
}

document.addEventListener('DOMContentLoaded', init);
