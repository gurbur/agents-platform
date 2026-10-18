'use strict';

// agents hub console (slice S11): the owner's chat console over the v1 API.
//
// The owner token lives in memory and, only when the user opts in, in this
// tab's sessionStorage; never in localStorage, cookies or the URL.
// localStorage holds view preferences only (chat style, last room, how many
// messages of each room were seen). Server data reaches the DOM as text
// nodes only: message bodies, names and intros are data, never markup.
// The console never moves a read marker.
//
// Slice S12 adds registering and deleting agents from the directory. The
// two one-time credentials of a registration live only in the text of the
// credential dialog while it is open and are wiped when it closes; they
// never reach storage, the URL or the console log.

const TOKEN_STORAGE_KEY = 'agents-platform.owner-token';
const PREF_STYLE = 'agents-console-style';
const PREF_ROOM = 'agents-console-room';
const PREF_SEEN = 'agents-console-seen';

const PAGE_SIZE = 50;
const POLL_PAGE_SIZE = 100;
const POLL_FEED_MS = 3000;
const POLL_ROOMS_MS = 10000;
const SEARCH_DEBOUNCE_MS = 350;
const GROUP_GAP_MS = 5 * 60 * 1000;
const DELIVERY_STALE_MS = 5000;
const INBOX_PAGE_SIZE = 20;
const OLDER_TRIGGER_PX = 120;
const BOTTOM_SLACK_PX = 48;
const MAX_POLL_PAGES = 5;
const MAX_JUMP_PAGES = 10;
const SNIPPET_CONCURRENCY = 4;
// Swipe-left-to-reply on touch: the drag claims the gesture past SWIPE_CLAIM_PX
// (horizontal over vertical), follows the finger up to SWIPE_MAX_PX, then
// resists; releasing past SWIPE_REPLY_PX starts the reply.
const SWIPE_CLAIM_PX = 10;
const SWIPE_REPLY_PX = 60;
const SWIPE_MAX_PX = 80;
const SWIPE_RESIST = 0.3;
const SWIPE_HAPTIC_MS = 10;
// platform-wire AVATAR_MAX_BYTES; the hub has the final word.
const AVATAR_MAX_BYTES = 512 * 1024;
// A body this long or with this many lines starts folded.
const FOLD_LINES = 4;
const FOLD_CHARS = 240;
const COLORS = ['#8fa5c2', '#a9bd92', '#e0a3b6', '#a7a3dc', '#d6b56e', '#8fc4bd', '#d79b84', '#b8aed0'];
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

const API = Object.freeze({
  owner: () => '/v1/owner',
  agents: () => '/v1/agents',
  agent: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}`,
  rooms: () => '/v1/rooms',
  messages: () => '/v1/messages',
  message: (messageId) => `/v1/messages/${encodeURIComponent(messageId)}`,
  roomPost: (roomId) => `/v1/rooms/${encodeURIComponent(roomId)}/messages`,
  profile: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/profile`,
  avatar: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/avatar`,
  receiver: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/receiver`,
  membership: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/membership/all`,
  delivery: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/delivery`,
  retry: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/delivery/retry`,
  inbox: (agentId) => `/v1/agents/${encodeURIComponent(agentId)}/inbox`,
});

const GENDER_LABEL = { female: '여성형', male: '남성형', none: '없음' };
const AGE_LABEL = { child: '어린이', teen: '10대', '20s': '20대', '30s_40s': '30~40대', '50s_plus': '50대 이상', none: '없음' };
const STATE_LABEL = { idle: 'idle', pending: 'pending', blocked: 'blocked' };
const NONE_TEXT = '—';

// The single piece of mutable state. `session` changes on connect and
// logout, `feedSeq` on every room switch, so late responses are dropped.
const S = {
  token: '',
  session: 0,
  me: null,
  postAs: [],
  agents: new Map(),
  directorySignature: '',
  rooms: [],
  extraRooms: [],
  snippets: new Map(),
  seen: {},
  room: null,
  feedSeq: 0,
  msgs: [],
  feedLoaded: false,
  feedError: null,
  hasOlder: false,
  olderCursor: null,
  olderBusy: false,
  pollBusy: false,
  newFrom: null,
  q: '',
  replyTo: null,
  expanded: new Set(),
  quotes: new Map(),
  style: 'bubble',
  edit: false,
  profileError: null,
  tab: {},
  member: null,
  infoTab: 'settings',
  delivery: new Map(),
  actionError: null,
  receiverEdit: null,
  inbox: new Map(),
  avatars: new Map(),
  avatarBusy: new Set(),
  avatarTarget: null,
  sending: false,
  regBusy: false,
  credFor: null,
  delTarget: null,
  delBusy: false,
  pendingSend: null,
  sendError: null,
  timers: [],
};

// ------------------------------------------------------------- DOM helpers

const $ = (id) => document.getElementById(id);
const app = $('app');
const SVG_NS = 'http://www.w3.org/2000/svg';

// Builds an element. `props`: class, text, data (dataset), bg and width
// (set through the CSSOM, which the CSP allows), anything else an attribute.
function h(tag, props, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props || {})) {
    if (value === null || value === undefined || value === false) continue;
    if (key === 'class') node.className = value;
    else if (key === 'text') node.textContent = value;
    else if (key === 'data') Object.assign(node.dataset, value);
    else if (key === 'bg') node.style.background = value;
    else if (key === 'width') node.style.width = value;
    else node.setAttribute(key, value === true ? '' : String(value));
  }
  for (const child of children.flat(Infinity)) {
    if (child === null || child === undefined || child === false || child === '') continue;
    node.append(child);
  }
  return node;
}

const ICONS = {
  hash: [['path', { d: 'M5 9h14M4 15h14M10 3 8 21M16 3l-2 18' }]],
  reply: [['path', { d: 'M9 14 4 9l5-5' }], ['path', { d: 'M4 9h10a6 6 0 0 1 6 6v4' }]],
  close: [['path', { d: 'M6 6l12 12M18 6 6 18' }]],
  back: [['path', { d: 'M15 18l-6-6 6-6' }]],
  logout: [['path', { d: 'M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3' }], ['path', { d: 'M10 17l-5-5 5-5M5 12h11' }]],
};

function icon(name) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('class', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('aria-hidden', 'true');
  for (const [tag, attrs] of ICONS[name]) {
    const part = document.createElementNS(SVG_NS, tag);
    for (const [key, value] of Object.entries(attrs)) part.setAttribute(key, value);
    svg.append(part);
  }
  return svg;
}

// --------------------------------------------------------------- utilities

function prefGet(key) {
  try {
    return window.localStorage.getItem(key);
  } catch (_) {
    return null;
  }
}

function prefSet(key, value) {
  try {
    window.localStorage.setItem(key, value);
  } catch (_) {
    // Storage may be unavailable (private mode); preferences stay in memory.
  }
}

function ulid() {
  let time = Date.now();
  let head = '';
  for (let index = 0; index < 10; index += 1) {
    head = CROCKFORD[time % 32] + head;
    time = Math.floor(time / 32);
  }
  const random = new Uint8Array(16);
  window.crypto.getRandomValues(random);
  let tail = '';
  for (const byte of random) tail += CROCKFORD[byte % 32];
  return head + tail;
}

const seqNum = (seq) => Number(seq);
const firstLine = (text) => String(text).split('\n')[0];
const failureText = (failure) => `${failure.code} — ${failure.message}`;

function clock(date) {
  const hours = date.getHours();
  const minutes = String(date.getMinutes()).padStart(2, '0');
  return `${hours < 12 ? '오전' : '오후'} ${hours % 12 || 12}:${minutes}`;
}

const shortClock = (date) => clock(date).replace(/^오[전후] /, '');
const dayKey = (date) => `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
const dayLabel = (date) => date.toLocaleDateString('ko-KR', { year: 'numeric', month: 'long', day: 'numeric', weekday: 'long' });

function relative(iso) {
  if (!iso) return '';
  const date = new Date(iso);
  const now = new Date();
  const minutes = Math.floor((now - date) / 60000);
  if (minutes < 1) return '방금';
  if (minutes < 60) return `${minutes}분`;
  if (dayKey(date) === dayKey(now)) return `${Math.floor(minutes / 60)}시간`;
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (dayKey(date) === dayKey(yesterday)) return '어제';
  if (date.getFullYear() === now.getFullYear()) return `${date.getMonth() + 1}월 ${date.getDate()}일`;
  return `${date.getFullYear()}.${date.getMonth() + 1}.${date.getDate()}`;
}

// The receiver URL with all but the first three host characters masked,
// keeping the scheme, port and path.
function maskedUrl(raw) {
  try {
    const url = new URL(raw);
    const host = url.hostname;
    const masked = `${host.slice(0, Math.min(3, Math.max(0, host.length - 3)))}••••••`;
    return `${url.protocol}//${masked}${url.port ? `:${url.port}` : ''}${url.pathname}`;
  } catch (_) {
    return '••••••';
  }
}

// --------------------------------------------------------------------- API

class ApiFailure extends Error {
  constructor(status, code, message, requestId) {
    super(message);
    this.status = status;
    this.code = code;
    this.requestId = requestId;
  }
}

function buildUrl(path, params) {
  const url = new URL(path, window.location.origin);
  for (const [name, value] of Object.entries(params || {})) {
    if (value !== null && value !== undefined && value !== '') url.searchParams.set(name, value);
  }
  return url;
}

async function readJson(response) {
  const text = await response.text();
  if (text === '') return null;
  try {
    return JSON.parse(text);
  } catch (_) {
    return null;
  }
}

async function failureFrom(response) {
  const body = await readJson(response);
  const error = body && body.error;
  if (error && typeof error.code === 'string') {
    return new ApiFailure(response.status, error.code, String(error.message), String(error.request_id || ''));
  }
  const requestId = response.headers.get('x-request-id') || '';
  return new ApiFailure(response.status, 'unexpected_response', response.statusText || 'no error body', requestId);
}

// The only way this page talks to the server. `json` sends a JSON body,
// `blob` a raw one (an avatar); `raw` returns the Response itself.
async function request(method, path, options = {}) {
  const session = S.session;
  const headers = {};
  if (S.token) headers.Authorization = `Bearer ${S.token}`;
  let body;
  if (options.json !== undefined) {
    headers['Content-Type'] = 'application/json';
    body = JSON.stringify(options.json);
  } else if (options.blob !== undefined) {
    headers['Content-Type'] = options.contentType;
    body = options.blob;
  }
  let response;
  try {
    response = await fetch(buildUrl(path, options.params), {
      method,
      headers,
      body,
      cache: 'no-store',
      credentials: 'omit',
      redirect: 'error',
    });
  } catch (error) {
    throw new ApiFailure(0, 'network_error', String(error.message || error), '');
  }
  if (session !== S.session) throw new ApiFailure(0, 'stale_session', 'session changed', '');
  if (!response.ok) {
    const failure = await failureFrom(response);
    if (failure.status === 401 && S.token) logout(`${failure.status} ${failureText(failure)}`);
    throw failure;
  }
  if (options.raw) return response;
  return readJson(response);
}

function asFailure(error) {
  if (error instanceof ApiFailure) return error;
  return new ApiFailure(0, 'client_error', String(error), '');
}

// ------------------------------------------------------------------ agents

const agentOf = (id) => S.agents.get(id) || null;
const nameOf = (id) => (agentOf(id) && agentOf(id).name) || id;

function colorOf(id) {
  let hash = 0;
  for (const char of id) hash = (hash * 31 + char.codePointAt(0)) >>> 0;
  return COLORS[hash % COLORS.length];
}

function initialOf(id) {
  const first = Array.from(nameOf(id).trim())[0] || '?';
  return first.toLocaleUpperCase();
}

function av(id, size) {
  const node = h('span', { class: `av s${size}`, data: { av: id }, bg: colorOf(id), 'aria-hidden': 'true' });
  paintAvatar(node, id);
  return node;
}

function paintAvatar(node, id) {
  const cached = S.avatars.get(id);
  const agent = agentOf(id);
  if (cached && agent && agent.avatar && agent.avatar.sha256 === cached.sha) {
    node.replaceChildren(h('img', { src: cached.url, alt: '' }));
  } else {
    node.replaceChildren(initialOf(id));
  }
}

function repaintAvatars(id) {
  for (const node of document.querySelectorAll(`.av[data-av="${id}"]`)) paintAvatar(node, id);
}

function forgetAvatar(id) {
  const cached = S.avatars.get(id);
  if (!cached) return;
  URL.revokeObjectURL(cached.url);
  S.avatars.delete(id);
}

// Fetches the avatars whose directory sha256 differs from the cached copy.
async function ensureAvatars() {
  for (const agent of S.agents.values()) {
    const id = agent.agent_id;
    const cached = S.avatars.get(id);
    if (!agent.avatar) {
      if (cached) {
        forgetAvatar(id);
        repaintAvatars(id);
      }
      continue;
    }
    if ((cached && cached.sha === agent.avatar.sha256) || S.avatarBusy.has(id)) continue;
    S.avatarBusy.add(id);
    loadAvatar(id, agent.avatar.sha256).finally(() => S.avatarBusy.delete(id));
  }
}

async function loadAvatar(id, sha) {
  try {
    const response = await request('GET', API.avatar(id), { raw: true });
    const blob = await response.blob();
    forgetAvatar(id);
    S.avatars.set(id, { sha, url: URL.createObjectURL(blob) });
    repaintAvatars(id);
  } catch (_) {
    // The initial stays; the next directory refresh tries again.
  }
}

function pill(agent, status) {
  const state = (status && status.state) || (agent && agent.delivery && agent.delivery.state) || null;
  if (!state) return h('span', { class: 'pill none', text: NONE_TEXT });
  if (state === 'no_receiver') {
    const label = agent && agent.agent_id === S.me ? '대상 아님' : 'receiver 없음';
    return h('span', { class: 'pill none', text: label });
  }
  return h('span', { class: `pill ${state}`, text: STATE_LABEL[state] || state });
}

// ------------------------------------------------------------------- rooms

function allRooms() {
  const known = new Set(S.rooms.map((room) => room.room_id));
  return S.rooms.concat(S.extraRooms.filter((room) => !known.has(room.room_id)));
}

const currentRoom = () => allRooms().find((room) => room.room_id === S.room) || null;

function roomKind(room) {
  if (room.kind === 'all') return 'all';
  return S.me && room.members.includes(S.me) ? 'dm' : 'pair';
}

const peerOf = (room) => room.members.find((id) => id !== S.me) || room.members[0];

function roomTitle(room) {
  const kind = roomKind(room);
  if (kind === 'all') return 'all';
  if (kind === 'dm') return nameOf(peerOf(room));
  return room.members.map(nameOf).join(' ↔ ');
}

function roomIcon(room, size) {
  const kind = roomKind(room);
  if (kind === 'all') return h('span', { class: 'hashbox' }, icon('hash'));
  if (kind === 'dm') return av(peerOf(room), size);
  return h('span', { class: 'pair' }, av(room.members[0], 20), av(room.members[1], 20));
}

function dmRoomId(a, b) {
  const [first, second] = [a, b].sort();
  return `dm:${first}:${second}`;
}

function unreadOf(room) {
  if (room.room_id === S.room) return 0;
  const seen = S.seen[room.room_id] || 0;
  return Math.max(0, room.message_count - seen);
}

function markSeen(room) {
  if (!room) return;
  const loaded = S.msgs.length;
  S.seen[room.room_id] = Math.max(S.seen[room.room_id] || 0, room.message_count, loaded);
  prefSet(PREF_SEEN, JSON.stringify(S.seen));
}

function renderRooms() {
  const query = $('roomQ').value.trim();
  const rooms = allRooms().filter((room) => !query || roomTitle(room).includes(query));
  const sections = [
    ['채널', rooms.filter((room) => roomKind(room) === 'all')],
    ['내 DM', rooms.filter((room) => roomKind(room) === 'dm')],
    ['에이전트끼리', rooms.filter((room) => roomKind(room) === 'pair')],
  ];
  const nodes = [];
  for (const [label, list] of sections) {
    if (list.length === 0) continue;
    nodes.push(h('div', { class: 'sec', text: label }));
    for (const room of list) nodes.push(roomRow(room));
  }
  if (nodes.length === 0) nodes.push(h('div', { class: 'empty', text: '맞는 방이 없습니다' }));
  $('rooms').replaceChildren(...nodes);
}

function roomRow(room) {
  const unread = unreadOf(room);
  return h('button', { class: 'room', data: { act: 'room', id: room.room_id }, 'aria-current': String(room.room_id === S.room) },
    h('span', { class: 'ic' }, roomIcon(room, 32)),
    h('span', { class: 'nm', text: roomTitle(room) }),
    h('span', { class: 'tm', text: relative(room.last_message_at) }),
    h('span', { class: 'sn', text: snippetText(room) }),
    unread > 0 ? h('span', { class: 'badge', text: String(unread) }) : h('span'));
}

function snippetText(room) {
  const snippet = S.snippets.get(room.room_id);
  if (!room.last_event_seq) return '아직 메시지가 없습니다';
  if (!snippet) return '';
  const who = roomKind(room) === 'dm'
    ? (snippet.sender === S.me ? '나: ' : '')
    : `${snippet.sender === S.me ? '나' : nameOf(snippet.sender)}: `;
  return who + firstLine(snippet.body);
}

// The last message of every room whose newest event_seq moved.
async function loadSnippets() {
  const stale = allRooms().filter((room) => room.last_event_seq
    && (S.snippets.get(room.room_id) || {}).seq !== room.last_event_seq);
  for (let start = 0; start < stale.length; start += SNIPPET_CONCURRENCY) {
    const chunk = stale.slice(start, start + SNIPPET_CONCURRENCY);
    await Promise.all(chunk.map(async (room) => {
      try {
        const page = await request('GET', API.messages(), { params: { room_id: room.room_id, limit: 1 } });
        const last = page.items[0];
        if (last) S.snippets.set(room.room_id, { seq: last.event_seq, sender: last.sender_id, body: last.body });
      } catch (_) {
        // Keep the old snippet; the next refresh retries.
      }
    }));
  }
  renderRooms();
}

function rememberSnippet(message) {
  const known = S.snippets.get(message.room_id);
  if (known && seqNum(known.seq) >= seqNum(message.event_seq)) return;
  S.snippets.set(message.room_id, { seq: message.event_seq, sender: message.sender_id, body: message.body });
}

// ----------------------------------------------------------------- top bar

function renderTop() {
  const room = currentRoom();
  if (!room) {
    $('ttl').replaceChildren();
    return;
  }
  const kind = roomKind(room);
  const sub = kind === 'all'
    ? `멤버 ${room.members.length}명 · 모두에게 전달`
    : kind === 'dm' ? `@${peerOf(room)} · 나와의 DM` : `에이전트끼리 · ${room.members.join(' ↔ ')}`;
  $('ttl').replaceChildren(
    kind === 'all' ? icon('hash') : roomIcon(room, 28),
    h('b', { text: roomTitle(room) }),
    h('span', { class: 'sub', text: sub }));
  $('infoBtn').classList.toggle('on', !app.classList.contains('info-off'));
}

function renderStyle() {
  app.classList.toggle('style-bubble', S.style === 'bubble');
  for (const button of document.querySelectorAll('.mode button')) {
    button.setAttribute('aria-pressed', String(button.dataset.style === S.style));
  }
  $('modeBtn').classList.toggle('on', S.style === 'bubble');
  $('modeBtn').setAttribute('aria-label', S.style === 'bubble'
    ? '보기 방식: 말풍선형 (누르면 디스코드형)'
    : '보기 방식: 디스코드형 (누르면 말풍선형)');
}

function renderMe() {
  const pillText = S.me ? `소유자 · ${nameOf(S.me)}` : '소유자';
  $('ownerPill').textContent = pillText;
  const logout = h('button', { class: 'btn icon', data: { act: 'logout' }, 'aria-label': '로그아웃', title: '로그아웃' }, icon('logout'));
  if (!S.me) {
    $('me').replaceChildren(h('span', { class: 'who' }, '소유자', h('small', { text: 'post_as 없음 · 읽기 전용' })), logout);
    return;
  }
  $('me').replaceChildren(av(S.me, 32), h('span', { class: 'who' }, nameOf(S.me), h('small', { text: `${S.me} · 소유자` })), logout);
}

// -------------------------------------------------------------------- feed

const feed = $('feed');
const findMessage = (id) => S.msgs.find((message) => message.message_id === id) || null;

function quoteTarget(id) {
  const loaded = findMessage(id);
  if (loaded) return loaded;
  const cached = S.quotes.get(id);
  if (cached) return cached;
  fetchQuote(id);
  return null;
}

async function fetchQuote(id) {
  S.quotes.set(id, { pending: true });
  const seq = S.feedSeq;
  try {
    S.quotes.set(id, await request('GET', API.message(id)));
  } catch (_) {
    S.quotes.set(id, { missing: true });
  }
  if (seq === S.feedSeq) rerenderFeed();
}

function highlighted(text) {
  if (!S.q) return [text];
  const parts = String(text).split(S.q);
  const nodes = [];
  parts.forEach((part, index) => {
    if (index > 0) nodes.push(h('mark', { text: S.q }));
    if (part) nodes.push(part);
  });
  return nodes;
}

function foldable(message) {
  return message.body.split('\n').length > FOLD_LINES || message.body.length > FOLD_CHARS;
}

function textBody(message) {
  const fold = foldable(message);
  const open = S.expanded.has(message.message_id);
  return [
    h('div', { class: `text${fold && !open ? ' clamp' : ''}` }, highlighted(message.body)),
    fold ? h('button', { class: 'more', data: { act: 'more', k: message.message_id }, text: open ? '접기' : '더 보기' }) : null,
  ];
}

function actions(message) {
  if (!S.me) return null;
  return h('div', { class: 'actions' },
    h('button', { data: { act: 'reply', k: message.message_id } }, icon('reply'), '답장'));
}

function isBargeIn(message, room) {
  return room.kind === 'dm' && !room.members.includes(message.sender_id);
}

function quoteParts(target) {
  if (!target || target.pending) return { who: '…', text: '원문을 불러오는 중' };
  if (target.missing) return { who: '', text: '원문을 찾을 수 없습니다' };
  return { who: nameOf(target.sender_id), text: firstLine(target.body), sender: target.sender_id };
}

function flatMessage(item, room) {
  const message = item.message;
  const quote = message.reply_to ? quoteParts(quoteTarget(message.reply_to)) : null;
  const date = new Date(message.created_at);
  const nodes = [];
  if (quote) {
    nodes.push(h('button', { class: 'quote', data: { act: 'jump', k: message.reply_to } },
      quote.sender ? av(quote.sender, 16) : null, quote.who ? h('b', { text: quote.who }) : null, h('span', { text: quote.text })));
  }
  if (item.head) {
    const name = h('span', { class: 'name', text: nameOf(message.sender_id), bg: null });
    name.style.color = colorOf(message.sender_id);
    nodes.push(av(message.sender_id, 36), h('div', null,
      h('div', { class: 'meta' }, name,
        isBargeIn(message, room) ? h('span', { class: 'tag', text: '끼어듦' }) : null,
        h('time', { datetime: message.created_at, text: clock(date) })),
      textBody(message)));
  } else {
    nodes.push(h('span', { class: 'gut', text: shortClock(date) }), h('div', null, textBody(message)));
  }
  nodes.push(actions(message));
  return h('div', { class: `msg${item.head ? ' head' : ''}`, id: `m-${message.message_id}` }, nodes);
}

function bubbleMessage(item, room) {
  const message = item.message;
  const mine = message.sender_id === S.me;
  const quote = message.reply_to ? quoteParts(quoteTarget(message.reply_to)) : null;
  const bq = quote ? h('button', { class: 'bq', data: { act: 'jump', k: message.reply_to } },
    quote.who ? h('b', { text: quote.who }) : null, h('span', { text: quote.text })) : null;
  let top = null;
  if (item.head && mine && isBargeIn(message, room)) top = h('div', { class: 'b-name' }, h('span', { class: 'tag', text: '끼어듦' }));
  if (item.head && !mine) top = h('div', { class: 'b-name', text: nameOf(message.sender_id) });
  const classes = ['b-row', mine ? 'mine' : 'other', item.head ? 'head' : '', item.tail ? 'tail' : ''].filter(Boolean).join(' ');
  return h('div', { class: classes, id: `m-${message.message_id}` },
    mine ? null : h('div', { class: 'b-av' }, item.head ? av(message.sender_id, 36) : null),
    h('div', { class: 'b-col' }, top,
      h('div', { class: 'b-line' },
        h('div', { class: 'bubble' }, bq, textBody(message)),
        item.tail ? h('time', { class: 'b-time', datetime: message.created_at, text: clock(new Date(message.created_at)) }) : null,
        actions(message))));
}

function renderFeed() {
  const room = currentRoom();
  if (!room) {
    feed.replaceChildren();
    return;
  }
  if (S.feedError) {
    feed.replaceChildren(h('div', { class: 'empty', text: `불러오지 못했습니다 — ${failureText(S.feedError)}` }));
    return;
  }
  if (!S.feedLoaded) {
    feed.replaceChildren(h('div', { class: 'empty', text: '불러오는 중…' }));
    return;
  }
  if (S.msgs.length === 0) {
    feed.replaceChildren(h('div', { class: 'empty', text: S.q ? `“${S.q}”와 맞는 메시지가 없습니다` : '아직 메시지가 없습니다' }));
    return;
  }
  const bubble = S.style === 'bubble';
  const items = S.msgs.map((message) => ({ message, date: new Date(message.created_at) }));
  items.forEach((item, index) => {
    const previous = items[index - 1];
    item.day = !previous || dayKey(previous.date) !== dayKey(item.date);
    item.fresh = !S.q && S.newFrom === item.message.message_id;
    // In the bubble style the quote sits inside the bubble, so a reply does
    // not break a group.
    item.head = item.day || item.fresh || previous.message.sender_id !== item.message.sender_id
      || (Boolean(item.message.reply_to) && !bubble) || item.date - previous.date > GROUP_GAP_MS;
  });
  items.forEach((item, index) => {
    item.tail = !items[index + 1] || items[index + 1].head;
  });
  const nodes = [];
  if (S.hasOlder) nodes.push(h('div', { class: 'older', text: S.olderBusy ? '이전 메시지를 불러오는 중…' : '위로 스크롤하면 이전 메시지를 불러옵니다' }));
  for (const item of items) {
    if (item.day) nodes.push(h('div', { class: 'day' }, h('span', { text: dayLabel(item.date) })));
    if (item.fresh) nodes.push(h('div', { class: 'newmark', text: '새 메시지' }));
    nodes.push(bubble ? bubbleMessage(item, room) : flatMessage(item, room));
  }
  feed.replaceChildren(...nodes);
  unfoldShortTexts();
}

// A folded text that fits in the clamp needs no "더 보기".
function unfoldShortTexts() {
  // A hidden feed (the phone's room list) has no layout to measure.
  if (feed.clientHeight === 0) return;
  for (const text of feed.querySelectorAll('.text.clamp')) {
    if (text.scrollHeight - text.clientHeight > 1) continue;
    text.classList.remove('clamp');
    const more = text.nextElementSibling;
    if (more && more.classList.contains('more')) more.remove();
  }
}

const nearBottom = () => feed.scrollHeight - feed.scrollTop - feed.clientHeight < BOTTOM_SLACK_PX;

function toBottom() {
  feed.style.scrollBehavior = 'auto';
  feed.scrollTop = feed.scrollHeight;
  feed.style.scrollBehavior = '';
}

// Re-renders without moving what the reader is looking at.
function rerenderFeed() {
  const atBottom = nearBottom();
  const top = feed.scrollTop;
  renderFeed();
  feed.style.scrollBehavior = 'auto';
  if (atBottom) feed.scrollTop = feed.scrollHeight;
  else feed.scrollTop = top;
  feed.style.scrollBehavior = '';
}

function mergeMessages(items) {
  const known = new Set(S.msgs.map((message) => message.message_id));
  const fresh = items.filter((message) => !known.has(message.message_id));
  if (fresh.length === 0) return 0;
  S.msgs = S.msgs.concat(fresh).sort((a, b) => seqNum(a.event_seq) - seqNum(b.event_seq));
  for (const message of fresh) rememberSnippet(message);
  return fresh.length;
}

function historyParams(extra) {
  return Object.assign({ room_id: S.room, q: S.q || null }, extra);
}

async function loadFeed() {
  const seq = S.feedSeq;
  try {
    const page = await request('GET', API.messages(), { params: historyParams({ limit: PAGE_SIZE }) });
    if (seq !== S.feedSeq) return;
    S.msgs = page.items.slice().reverse();
    S.hasOlder = page.has_more;
    S.olderCursor = page.next_before_event_seq || null;
    S.feedError = null;
  } catch (error) {
    if (seq !== S.feedSeq) return;
    S.feedError = asFailure(error);
  }
  S.feedLoaded = true;
}

async function loadOlder() {
  if (!S.hasOlder || S.olderBusy || !S.olderCursor) return false;
  S.olderBusy = true;
  const seq = S.feedSeq;
  try {
    const page = await request('GET', API.messages(), { params: historyParams({ before_event_seq: S.olderCursor, limit: PAGE_SIZE }) });
    if (seq !== S.feedSeq) return false;
    const fromBottom = feed.scrollHeight - feed.scrollTop;
    mergeMessages(page.items);
    S.hasOlder = page.has_more;
    S.olderCursor = page.next_before_event_seq || null;
    S.olderBusy = false;
    renderFeed();
    feed.style.scrollBehavior = 'auto';
    feed.scrollTop = feed.scrollHeight - fromBottom;
    feed.style.scrollBehavior = '';
    return true;
  } catch (_) {
    return false;
  } finally {
    S.olderBusy = false;
  }
}

function newestSeq() {
  const last = S.msgs[S.msgs.length - 1];
  return last ? last.event_seq : '0';
}

async function pollFeed() {
  if (!S.token || !S.room || !S.feedLoaded || S.feedError || S.pollBusy || document.hidden) return;
  S.pollBusy = true;
  const seq = S.feedSeq;
  let added = 0;
  try {
    let after = newestSeq();
    for (let page = 0; page < MAX_POLL_PAGES; page += 1) {
      const result = await request('GET', API.messages(), { params: historyParams({ after_event_seq: after, limit: POLL_PAGE_SIZE }) });
      if (seq !== S.feedSeq) return;
      added += mergeMessages(result.items);
      if (!result.has_more || !result.next_after_event_seq) break;
      after = result.next_after_event_seq;
    }
  } catch (_) {
    // The next tick retries.
  } finally {
    S.pollBusy = false;
  }
  if (added === 0 || seq !== S.feedSeq) return;
  markSeen(currentRoom());
  rerenderFeed();
  renderRooms();
}

async function jumpTo(id) {
  let target = $(`m-${id}`);
  for (let page = 0; !target && page < MAX_JUMP_PAGES; page += 1) {
    if (!(await loadOlder())) break;
    target = $(`m-${id}`);
  }
  if (!target) return;
  target.scrollIntoView({ block: 'center', behavior: 'smooth' });
  target.classList.remove('flash');
  void target.offsetWidth;
  target.classList.add('flash');
}

// ---------------------------------------------------------------- swipe to reply

function startReply(id) {
  S.replyTo = id;
  renderComposer();
  $('input').focus();
}

let swipe = null;

function swipeOffset(dx) {
  const pull = Math.max(0, -dx);
  const travel = pull <= SWIPE_MAX_PX ? pull : SWIPE_MAX_PX + (pull - SWIPE_MAX_PX) * SWIPE_RESIST;
  return -travel;
}

function onSwipeDown(event) {
  if (event.pointerType !== 'touch' || !event.isPrimary || !S.me) return;
  const row = event.target.closest('.msg, .b-row');
  if (!row || !row.id.startsWith('m-')) return;
  swipe = { row, id: row.id.slice(2), x: event.clientX, y: event.clientY, claimed: false, offset: 0, icon: null };
}

function claimSwipe(state) {
  state.claimed = true;
  state.row.classList.remove('swipe-back');
  state.row.classList.add('swiping');
  state.icon = h('span', { class: 'swipe-ico', 'aria-hidden': 'true' }, icon('reply'));
  state.row.append(state.icon);
}

function onSwipeMove(event) {
  if (!swipe || event.pointerType !== 'touch' || !event.isPrimary) return;
  if (!swipe.row.isConnected) {
    swipe = null;
    return;
  }
  const dx = event.clientX - swipe.x;
  const dy = event.clientY - swipe.y;
  if (!swipe.claimed) {
    // Until the move is clearly a leftward horizontal drag the browser keeps
    // the gesture (scroll, long-press selection).
    if (Math.abs(dy) > SWIPE_CLAIM_PX && Math.abs(dy) >= Math.abs(dx)) {
      swipe = null;
      return;
    }
    if (dx >= -SWIPE_CLAIM_PX || Math.abs(dx) <= Math.abs(dy)) return;
    claimSwipe(swipe);
  }
  swipe.offset = swipeOffset(dx);
  swipe.row.style.transform = `translateX(${swipe.offset}px)`;
  // The icon stays put at the right edge while the row slides over it.
  swipe.icon.style.transform = `translateX(${-swipe.offset}px)`;
  swipe.icon.style.opacity = String(Math.min(1, -swipe.offset / SWIPE_REPLY_PX));
}

function settleSwipe(state) {
  const { row } = state;
  row.classList.remove('swiping');
  row.classList.add('swipe-back');
  row.style.transform = '';
  if (state.icon) state.icon.remove();
  row.addEventListener('transitionend', () => row.classList.remove('swipe-back'), { once: true });
}

// The finger lifting off a swiped quote or "더 보기" must not also click it.
function swallowNextClick() {
  const swallow = (event) => {
    event.stopPropagation();
    event.preventDefault();
  };
  document.addEventListener('click', swallow, { capture: true, once: true });
  window.setTimeout(() => document.removeEventListener('click', swallow, { capture: true }), 400);
}

function onSwipeEnd(event) {
  if (!swipe || event.pointerType !== 'touch' || !event.isPrimary) return;
  const state = swipe;
  swipe = null;
  if (!state.claimed) return;
  settleSwipe(state);
  swallowNextClick();
  if (event.type !== 'pointerup' || -state.offset < SWIPE_REPLY_PX) return;
  if (navigator.vibrate) navigator.vibrate(SWIPE_HAPTIC_MS);
  startReply(state.id);
}

function bindSwipe() {
  feed.addEventListener('pointerdown', onSwipeDown, { passive: true });
  feed.addEventListener('pointermove', onSwipeMove, { passive: true });
  feed.addEventListener('pointerup', onSwipeEnd, { passive: true });
  feed.addEventListener('pointercancel', onSwipeEnd, { passive: true });
}

// ---------------------------------------------------------------- composer

function renderComposer() {
  const room = currentRoom();
  const strips = [];
  if (!S.me) {
    strips.push(h('div', { class: 'strip note' }, h('span', { class: 'dot' }),
      h('span', { text: '보낼 명의가 없습니다. 설정 [owner] post_as가 비어 있어 읽기만 합니다.' })));
  } else if (room && roomKind(room) === 'pair') {
    strips.push(h('div', { class: 'strip' }, h('span', { class: 'dot' }),
      h('span', null, h('b', { text: S.me }), '으로 끼어들어 말합니다')));
  }
  const target = S.replyTo && (findMessage(S.replyTo) || S.quotes.get(S.replyTo));
  if (target && target.sender_id) {
    strips.push(h('div', { class: 'strip' },
      h('span', null, h('b', { text: nameOf(target.sender_id) }), '에게 답장'),
      h('button', { class: 'x', data: { act: 'unreply' }, 'aria-label': '답장 취소' }, icon('close'))));
  }
  if (S.sendError) {
    strips.push(h('div', { class: 'strip bad', role: 'alert' }, h('span', { class: 'dot' }),
      h('span', null, h('b', { text: S.sendError.code }), ` — ${S.sendError.message}`),
      h('button', { class: 'x', data: { act: 'dismiss-send' }, 'aria-label': '오류 닫기' }, icon('close'))));
  }
  $('strips').replaceChildren(...strips);
  const input = $('input');
  input.disabled = !S.me;
  input.placeholder = S.me ? '메시지 보내기' : '보낼 명의가 없습니다';
  updateSendButton();
}

function updateSendButton() {
  const ready = Boolean(S.me) && !S.sending && Boolean($('input').value.trim());
  $('send').classList.toggle('ready', ready);
  $('send').disabled = !S.me || S.sending;
}

function grow() {
  const box = $('input');
  box.style.height = 'auto';
  box.style.height = `${box.scrollHeight}px`;
  updateSendButton();
}

async function send() {
  const room = currentRoom();
  const box = $('input');
  const text = box.value.trim();
  if (!text || !S.me || !room || S.sending) return;
  const replyTo = S.replyTo;
  // A retry of the same text into the same room keeps its key, so a lost
  // response cannot post the message twice.
  const pending = S.pendingSend;
  const same = pending && pending.room === room.room_id && pending.text === text && pending.replyTo === replyTo;
  const clientKey = same ? pending.key : `console-${ulid()}`;
  S.pendingSend = { room: room.room_id, text, replyTo, key: clientKey };
  S.sending = true;
  updateSendButton();
  const body = { sender_id: S.me, body: text, client_key: clientKey };
  if (replyTo) body.reply_to = replyTo;
  try {
    const message = await request('POST', API.roomPost(room.room_id), { json: body });
    S.pendingSend = null;
    S.sendError = null;
    S.replyTo = null;
    S.newFrom = null;
    box.value = '';
    grow();
    if (S.q) {
      S.q = '';
      $('histQ').value = '';
      await reloadRoom();
    } else if (S.room === message.room_id) {
      mergeMessages([message]);
      renderFeed();
    }
    renderComposer();
    toBottom();
    refreshRooms();
  } catch (error) {
    S.sendError = asFailure(error);
    renderComposer();
  } finally {
    S.sending = false;
    updateSendButton();
  }
}

// -------------------------------------------------------------------- info

function infoAgentId(room) {
  const kind = roomKind(room);
  if (kind === 'all') return S.member;
  if (kind === 'dm') return peerOf(room);
  return S.tab[room.room_id] || room.members[0];
}

function renderInfo() {
  const room = currentRoom();
  const nodes = [];
  if (!room) {
    $('infoBody').replaceChildren();
    return;
  }
  const kind = roomKind(room);
  if (kind === 'all' && !S.member) {
    const block = h('div', { class: 'blk' }, h('h3', { text: `멤버 ${room.members.length}` }));
    for (const id of room.members) {
      const agent = agentOf(id);
      block.append(h('button', { class: 'member', data: { act: 'member', id } }, av(id, 32),
        h('span', { class: 'who' }, nameOf(id), h('small', { text: (agent && agent.profile.role) || id })), pill(agent)));
    }
    if (room.members.length === 0) block.append(h('p', { class: 'muted flush', text: '가입한 에이전트가 없습니다' }));
    nodes.push(block);
  } else if (kind === 'all') {
    nodes.push(h('button', { class: 'linkback', data: { act: 'member', id: '' } }, icon('back'), '멤버'));
    nodes.push(...agentBlock(S.member));
  } else if (kind === 'dm') {
    nodes.push(...agentBlock(peerOf(room)));
  } else {
    const current = infoAgentId(room);
    nodes.push(h('div', { class: 'seg', role: 'tablist' }, room.members.map((id) => h('button', {
      role: 'tab', 'aria-selected': String(id === current), data: { act: 'tab', id },
    }, av(id, 16), nameOf(id)))));
    nodes.push(...agentBlock(current));
  }
  $('infoBody').replaceChildren(...nodes);
}

// Poll-driven renders skip the panel while the user is typing into it.
function renderInfoPassively() {
  const active = document.activeElement;
  if (S.edit || S.receiverEdit || (active && $('info').contains(active) && active.matches('input, textarea, select'))) return;
  renderInfo();
}

function agentBlock(id) {
  const agent = agentOf(id);
  if (!agent) {
    return [h('div', { class: 'blk' }, h('div', { class: 'prof' }, av(id, 56), h('div', null, h('div', { class: 'nm', text: id }), h('div', { class: 'id', text: id }))),
      h('p', { class: 'muted flush' }, h('b', { text: '등록되지 않은 agent' }), ' — 삭제되었거나 등록된 적 없는 ID입니다. 남은 대화는 그대로 읽을 수 있습니다.'))];
  }
  const nodes = [profileBlock(agent)];
  nodes.push(h('div', { class: 'seg sub', role: 'tablist' },
    h('button', { role: 'tab', 'aria-selected': String(S.infoTab === 'settings'), data: { act: 'info-tab', tab: 'settings' }, text: '전달·설정' }),
    h('button', { role: 'tab', 'aria-selected': String(S.infoTab === 'inbox'), data: { act: 'info-tab', tab: 'inbox' }, text: 'inbox' })));
  if (S.infoTab === 'inbox') {
    nodes.push(inboxBlock(agent));
    return nodes;
  }
  const status = deliveryOf(id);
  if (id === S.me && agent.receiver_url === null) {
    nodes.push(h('div', { class: 'blk' }, h('h3', { text: '전달' }),
      h('p', { class: 'muted flush', text: '소유자는 receiver 알림 대상이 아닙니다. 브라우저와 CLI로 직접 읽습니다.' })));
  } else {
    nodes.push(deliveryBlock(agent, status));
  }
  nodes.push(receiverBlock(agent), joinBlock(agent));
  if (S.actionError && S.actionError.id === id) {
    nodes.push(h('div', { class: 'blk' }, h('div', { class: 'err', role: 'alert', text: failureText(S.actionError.failure) })));
  }
  return nodes;
}

function profileBlock(agent) {
  const id = agent.agent_id;
  const profile = agent.profile;
  const field = (key, label, control, shown) => [h('dt', { text: label }), h('dd', null, S.edit ? control : shown)];
  const select = (key, labels, value) => h('select', { id: `f-${key}` },
    h('option', { value: '', text: '선언 안 함', selected: value === null }),
    Object.entries(labels).map(([option, text]) => h('option', { value: option, text, selected: value === option })));
  const rows = [];
  if (S.edit) rows.push(field('name', '이름', h('input', { id: 'f-name', value: agent.name || '' }), null));
  rows.push(field('gender', '성별형', select('gender_form', GENDER_LABEL, profile.gender_form), profile.gender_form ? GENDER_LABEL[profile.gender_form] : NONE_TEXT));
  rows.push(field('age', '나이대', select('age_band', AGE_LABEL, profile.age_band), profile.age_band ? AGE_LABEL[profile.age_band] : NONE_TEXT));
  rows.push(field('role', '역할', h('input', { id: 'f-role', value: profile.role || '' }), profile.role || NONE_TEXT));
  const intro = h('textarea', { id: 'f-intro' });
  intro.value = profile.intro || '';
  rows.push(field('intro', '소개', intro, profile.intro || NONE_TEXT));
  return h('div', { class: 'blk' },
    h('div', { class: 'prof' }, av(id, 56), h('div', null, h('div', { class: 'nm', text: agent.name || id }), h('div', { class: 'id', text: id }))),
    S.edit ? h('div', { class: 'row-start' },
      h('button', { class: 'btn sm', data: { act: 'avatar-upload', id }, text: '사진 바꾸기' }),
      h('button', { class: 'btn sm', data: { act: 'avatar-clear', id }, disabled: !agent.avatar, text: '사진 지우기' })) : null,
    h('dl', { class: 'kv' }, rows),
    h('div', { class: 'row-end' }, S.edit
      ? [h('button', { class: 'btn sm', data: { act: 'edit-cancel' }, text: '취소' }), h('button', { class: 'btn sm primary', data: { act: 'edit-save', id }, text: '저장' })]
      : h('button', { class: 'btn sm', data: { act: 'edit' }, text: '편집' })),
    S.profileError ? h('div', { class: 'err', role: 'alert', text: S.profileError }) : null);
}

function deliveryOf(id) {
  const entry = S.delivery.get(id);
  if (!entry || Date.now() - entry.at > DELIVERY_STALE_MS) refreshDelivery(id);
  return entry ? entry.status : null;
}

async function refreshDelivery(id) {
  const entry = S.delivery.get(id);
  if (entry && entry.busy) return;
  S.delivery.set(id, Object.assign({ status: null, at: 0 }, entry, { busy: true, at: Date.now() }));
  try {
    const status = await request('GET', API.delivery(id));
    S.delivery.set(id, { status, at: Date.now(), busy: false });
  } catch (_) {
    S.delivery.set(id, Object.assign({}, S.delivery.get(id), { busy: false }));
    return;
  }
  if (infoAgentIdNow() === id) renderInfoPassively();
}

function infoAgentIdNow() {
  const room = currentRoom();
  return room ? infoAgentId(room) : null;
}

function deliveryBlock(agent, status) {
  const read = status ? seqNum(status.read_marker) : null;
  const head = status ? seqNum(status.inbox_head_seq) : null;
  const percent = status && head > 0 ? Math.round((read / head) * 100) : 100;
  const state = (status && status.state) || agent.delivery.state;
  const last = status && status.last_attempt_at
    ? `${relative(status.last_attempt_at)}${status.last_error ? ` · ${status.last_error}` : ''}`
    : NONE_TEXT;
  return h('div', { class: 'blk' },
    h('h3', null, '전달 상태 ', pill(agent, status)),
    h('div', { class: 'stat' },
      h('span', { text: '읽음 / head' }), h('span', { class: 'num', text: status ? `${read} / ${head}` : NONE_TEXT }),
      h('span', { text: '미확인' }), h('span', { class: 'num', text: status ? String(head - read) : NONE_TEXT }),
      h('span', { text: '마지막 시도' }), h('span', { text: last })),
    h('div', { class: 'bar' }, h('i', { width: `${percent}%` })),
    state === 'blocked' ? h('div', { class: 'err', text: `전달이 멈췄습니다${status && status.last_error ? ` (${status.last_error})` : ''}. receiver를 고치거나 retry로 다시 시도합니다.` }) : null,
    h('div', { class: 'row-end' }, h('button', { class: 'btn sm', data: { act: 'retry', id: agent.agent_id }, disabled: state === 'idle' || state === 'no_receiver', text: 'retry' })));
}

function receiverBlock(agent) {
  const id = agent.agent_id;
  if (S.receiverEdit === id) {
    return h('div', { class: 'blk' }, h('h3', { text: 'receiver' }),
      h('div', { class: 'url' }, h('input', { id: 'recvInput', placeholder: 'receiver URL (http·https)', spellcheck: 'false', autocomplete: 'off' })),
      h('div', { class: 'row-end' },
        h('button', { class: 'btn sm', data: { act: 'receiver-cancel' }, text: '취소' }),
        h('button', { class: 'btn sm primary', data: { act: 'receiver-save', id }, text: '저장' })));
  }
  return h('div', { class: 'blk' }, h('h3', { text: 'receiver' }),
    h('div', { class: 'url' }, h('span', { text: agent.receiver_url ? maskedUrl(agent.receiver_url) : '연결 안 됨' })),
    h('div', { class: 'row-end' },
      agent.receiver_url ? h('button', { class: 'btn sm', data: { act: 'receiver-clear', id }, text: '끊기' }) : null,
      h('button', { class: 'btn sm', data: { act: 'receiver-edit', id }, text: agent.receiver_url ? '바꾸기' : '설정' })));
}

function joinBlock(agent) {
  return h('div', { class: 'blk' }, h('div', { class: 'toggle-row' },
    h('span', null, 'all 가입', h('small', { text: agent.joined_all ? '# all 메시지를 받습니다' : '# all 메시지를 받지 않습니다' })),
    h('button', { class: 'switch', role: 'switch', 'aria-checked': String(agent.joined_all), 'aria-label': 'all 가입', data: { act: 'join', id: agent.agent_id } })));
}

function inboxBlock(agent) {
  const id = agent.agent_id;
  const state = S.inbox.get(id);
  if (!state) loadInbox(id, false);
  const block = h('div', { class: 'blk' }, h('h3', null, 'inbox',
    state && state.readMarker !== null ? h('span', { class: 'num', text: `읽음 ${state.readMarker} · head ${state.head}` }) : null));
  block.append(h('p', { class: 'muted flush', text: '읽음 위치 뒤의 항목입니다. 콘솔은 읽음 위치를 바꾸지 않습니다.' }));
  if (!state || (state.busy && state.items.length === 0)) {
    block.append(h('p', { class: 'muted', text: '불러오는 중…' }));
    return block;
  }
  if (state.error) block.append(h('div', { class: 'err', text: failureText(state.error) }));
  const list = h('div');
  for (const item of state.items) {
    const summary = allRooms().find((room) => room.room_id === item.room_id);
    list.append(h('div', { class: 'ibx' },
      h('span', { class: 'num', text: `#${item.inbox_seq}` }),
      h('div', { class: 'hd' }, h('b', { text: nameOf(item.sender_id) }), h('span', { text: `${summary ? roomTitle(summary) : item.room_id} · ${relative(item.created_at)}` })),
      h('div', { class: 'bd', text: firstLine(item.body) })));
  }
  if (state.items.length === 0 && !state.error) list.append(h('p', { class: 'muted', text: '미확인 항목이 없습니다' }));
  block.append(list, h('div', { class: 'row-end' },
    h('button', { class: 'btn sm', data: { act: 'inbox-refresh', id }, text: '새로고침' }),
    state.hasMore ? h('button', { class: 'btn sm', data: { act: 'inbox-more', id }, disabled: state.busy, text: '더 보기' }) : null));
  return block;
}

async function loadInbox(id, more) {
  const previous = S.inbox.get(id);
  if (previous && previous.busy) return;
  const state = more && previous
    ? Object.assign({}, previous, { busy: true })
    : { items: [], readMarker: null, head: null, until: null, nextAfter: null, hasMore: false, error: null, busy: true };
  S.inbox.set(id, state);
  const params = more ? { after_seq: state.nextAfter, until_seq: state.until, limit: INBOX_PAGE_SIZE } : { limit: INBOX_PAGE_SIZE };
  try {
    const page = await request('GET', API.inbox(id), { params });
    state.items = state.items.concat(page.items);
    state.readMarker = page.read_marker;
    state.head = page.snapshot_head_seq;
    state.until = page.snapshot_head_seq;
    state.nextAfter = page.next_after_seq;
    state.hasMore = page.has_more;
    state.error = null;
  } catch (error) {
    state.error = asFailure(error);
  }
  state.busy = false;
  if (infoAgentIdNow() === id) renderInfo();
}

// ------------------------------------------------------------- owner writes

async function saveProfile(id) {
  const agent = agentOf(id);
  if (!agent) return;
  const value = (key) => $(`f-${key}`).value.trim();
  const orNull = (text) => (text === '' ? null : text);
  const next = {
    name: value('name'),
    gender_form: orNull(value('gender_form')),
    age_band: orNull(value('age_band')),
    role: orNull(value('role')),
    intro: orNull($('f-intro').value.trim()),
  };
  const body = {};
  if (next.name !== '' && next.name !== agent.name) body.name = next.name;
  for (const key of ['gender_form', 'age_band', 'role', 'intro']) {
    if (next[key] !== agent.profile[key]) body[key] = next[key];
  }
  if (Object.keys(body).length === 0) {
    S.edit = false;
    S.profileError = null;
    renderInfo();
    return;
  }
  try {
    const saved = await request('PUT', API.profile(id), { json: body });
    S.agents.set(id, Object.assign({}, agent, { name: saved.name, profile: saved.profile }));
    S.edit = false;
    S.profileError = null;
    renderAll();
  } catch (error) {
    S.profileError = failureText(asFailure(error));
    renderInfo();
  }
}

async function uploadAvatar(id, file) {
  S.profileError = null;
  if (file.size > AVATAR_MAX_BYTES) {
    S.profileError = `avatar_too_large — 사진은 ${AVATAR_MAX_BYTES / 1024} KiB 이하만 올릴 수 있습니다`;
    renderInfo();
    return;
  }
  try {
    const stored = await request('PUT', API.avatar(id), { blob: file, contentType: file.type || 'application/octet-stream' });
    updateAgent(id, { avatar: { sha256: stored.avatar.sha256, updated_at: stored.avatar.updated_at } });
    ensureAvatars();
  } catch (error) {
    S.profileError = failureText(asFailure(error));
  }
  renderInfo();
}

async function clearAvatar(id) {
  S.profileError = null;
  try {
    await request('DELETE', API.avatar(id));
    updateAgent(id, { avatar: null });
    forgetAvatar(id);
    repaintAvatars(id);
  } catch (error) {
    S.profileError = failureText(asFailure(error));
  }
  renderInfo();
}

function updateAgent(id, changes) {
  const agent = agentOf(id);
  if (agent) S.agents.set(id, Object.assign({}, agent, changes));
}

// Runs an owner write from the info panel and shows its error there.
async function ownerAction(id, work) {
  S.actionError = null;
  try {
    await work();
  } catch (error) {
    S.actionError = { id, failure: asFailure(error) };
  }
  S.delivery.delete(id);
  await refreshDirectory();
  renderInfo();
}

function saveReceiver(id) {
  const url = $('recvInput').value.trim();
  if (!url) return;
  S.receiverEdit = null;
  ownerAction(id, () => request('PATCH', API.receiver(id), { json: { receiver_url: url } }));
}

function clearReceiver(id) {
  if (!window.confirm(`${nameOf(id)}의 receiver를 끊을까요? 알림이 멈추고 URL은 다시 입력해야 합니다.`)) return;
  ownerAction(id, () => request('PATCH', API.receiver(id), { json: { receiver_url: null } }));
}

function toggleJoin(id) {
  const agent = agentOf(id);
  if (!agent) return;
  ownerAction(id, async () => {
    await request('PUT', API.membership(id), { json: { joined: !agent.joined_all } });
    await refreshRooms();
  });
}

function retryDelivery(id) {
  ownerAction(id, () => request('POST', API.retry(id)));
}

// --------------------------------------------------------------- directory

function renderDir() {
  const agents = Array.from(S.agents.values());
  $('dirCount').textContent = `에이전트 ${agents.length}`;
  $('dirBody').replaceChildren(...agents.map((agent) => h('tr', agent.agent_id === S.me || !S.me ? null : { data: { act: 'dm-open', id: agent.agent_id }, title: `${agent.name || agent.agent_id}와의 DM 열기` },
    h('td', null, h('span', { class: 'cell' }, av(agent.agent_id, 28), h('span', null, agent.name || agent.agent_id, ' ', h('span', { class: 'id', text: agent.agent_id })))),
    h('td', { text: agent.profile.role || NONE_TEXT }),
    h('td', { text: agent.joined_all ? '예' : '아니요' }),
    h('td', null, pill(agent)),
    h('td', { class: 'act' }, S.postAs.includes(agent.agent_id)
      ? h('span', { class: 'pill none', title: 'owner 게시 명의([owner] post_as)라 지울 수 없습니다', text: '보호됨' })
      : h('button', { class: 'btn sm ghost-bad', data: { act: 'del-open', id: agent.agent_id }, text: '삭제' })))));
}

// ------------------------------------------------- registration, deletion

function openRegister() {
  $('regForm').reset();
  showFormError('regError', null);
  $('reg').showModal();
  $('regId').focus();
}

function showFormError(id, text) {
  $(id).hidden = !text;
  $(id).textContent = text || '';
}

async function submitRegister() {
  if (S.regBusy) return;
  const body = { agent_id: $('regId').value.trim(), name: $('regName').value.trim() };
  const receiverUrl = $('regUrl').value.trim();
  if (receiverUrl) body.receiver_url = receiverUrl;
  S.regBusy = true;
  $('regGo').disabled = true;
  showFormError('regError', null);
  let created;
  try {
    created = await request('POST', API.agents(), { json: body });
  } catch (error) {
    showFormError('regError', failureText(asFailure(error)));
    return;
  } finally {
    S.regBusy = false;
    $('regGo').disabled = false;
  }
  $('reg').close();
  showCredentials(created);
}

// The response is shown once and kept nowhere else: the dialog's text is
// the only copy, and closing the dialog (button, Escape or logout) wipes it.
function showCredentials(created) {
  S.credFor = created.agent_id;
  $('credId').textContent = created.agent_id;
  $('credKey').textContent = created.agent_api_key;
  $('credSecret').textContent = created.receiver_secret;
  for (const button of $('cred').querySelectorAll('[data-act="copy"]')) button.textContent = '복사';
  $('cred').showModal();
}

async function onCredentialsClosed() {
  $('credKey').textContent = '';
  $('credSecret').textContent = '';
  const id = S.credFor;
  S.credFor = null;
  if (!id || !S.token) return;
  await refreshDirectory();
  renderDir();
  if (S.me && agentOf(id)) openDm(id);
}

// The Clipboard API needs a secure context, which a plain-HTTP tailnet origin
// is not; then the value is selected for a manual copy instead.
async function copySecret(button) {
  const node = $(button.dataset.k);
  try {
    await navigator.clipboard.writeText(node.textContent);
    button.textContent = '복사됨';
    return;
  } catch (_) {
    window.getSelection().selectAllChildren(node);
  }
  button.textContent = document.execCommand('copy') ? '복사됨' : '선택됨';
}

function openDelete(id) {
  S.delTarget = id;
  $('delId').textContent = id;
  const agent = agentOf(id);
  $('delWho').replaceChildren(av(id, 36), h('div', null, h('div', { class: 'nm', text: (agent && agent.name) || id }), h('div', { class: 'id', text: id })));
  $('delInput').value = '';
  $('delGo').disabled = true;
  showFormError('delError', null);
  $('del').showModal();
  $('delInput').focus();
}

function onDeleteInput() {
  $('delGo').disabled = S.delBusy || $('delInput').value.trim() !== S.delTarget;
}

function deleteFailureText(id, failure) {
  if (failure.code === 'agent_protected') {
    return `${id}는 owner 게시 명의([owner] post_as)라 지울 수 없습니다. 설정에서 빼고 platformd를 다시 시작한 뒤 지우세요. (${failureText(failure)})`;
  }
  return failureText(failure);
}

async function confirmDelete() {
  const id = S.delTarget;
  if (S.delBusy || !id || $('delInput').value.trim() !== id) return;
  S.delBusy = true;
  onDeleteInput();
  try {
    await request('DELETE', API.agent(id));
  } catch (error) {
    showFormError('delError', deleteFailureText(id, asFailure(error)));
    return;
  } finally {
    S.delBusy = false;
    onDeleteInput();
  }
  $('del').close();
  await afterDelete(id);
}

// The agent's messages and rooms stay; a room it was in closes if open.
async function afterDelete(id) {
  forgetAvatar(id);
  S.delivery.delete(id);
  S.inbox.delete(id);
  S.extraRooms = S.extraRooms.filter((room) => !room.members.includes(id));
  const room = currentRoom();
  const closing = room && room.kind === 'dm' && room.members.includes(id);
  if (S.member === id) S.member = null;
  await refreshDirectory();
  await refreshRooms();
  renderDir();
  if (closing) await openRoom('all', false);
  else renderInfo();
}

function openDm(id) {
  if (!S.me || id === S.me) return;
  const roomId = dmRoomId(S.me, id);
  if (!allRooms().some((room) => room.room_id === roomId)) {
    S.extraRooms.push({
      room_id: roomId, kind: 'dm', members: [S.me, id].sort(),
      last_event_seq: null, last_message_at: null, last_sender_id: null, message_count: 0,
    });
  }
  $('dir').close();
  openRoom(roomId);
}

// ------------------------------------------------------------ data refresh

async function refreshDirectory() {
  try {
    const directory = await request('GET', API.agents());
    S.agents = new Map(directory.items.map((item) => [item.agent_id, item]));
  } catch (_) {
    return;
  }
  const signature = JSON.stringify(Array.from(S.agents.values()).map((agent) => [agent.agent_id, agent.name, agent.avatar && agent.avatar.sha256]));
  ensureAvatars();
  if (signature === S.directorySignature) return;
  S.directorySignature = signature;
  renderMe();
  renderTop();
  renderRooms();
  rerenderFeed();
  renderInfoPassively();
}

async function refreshRooms() {
  let list;
  try {
    list = await request('GET', API.rooms());
  } catch (_) {
    return;
  }
  S.rooms = list.rooms;
  const known = new Set(S.rooms.map((room) => room.room_id));
  S.extraRooms = S.extraRooms.filter((room) => !known.has(room.room_id));
  const room = currentRoom();
  if (room && !document.hidden) markSeen(room);
  renderRooms();
  renderTop();
  loadSnippets();
  if (room && room.last_event_seq && seqNum(room.last_event_seq) > seqNum(newestSeq())) pollFeed();
}

async function refreshAll() {
  if (!S.token || document.hidden) return;
  await refreshDirectory();
  await refreshRooms();
  const id = infoAgentIdNow();
  if (id) refreshDelivery(id);
}

// ------------------------------------------------------------------ rooms

function resetRoomState() {
  S.feedSeq += 1;
  S.msgs = [];
  S.feedLoaded = false;
  S.feedError = null;
  S.hasOlder = false;
  S.olderCursor = null;
  S.olderBusy = false;
  S.newFrom = null;
  S.expanded.clear();
}

// `enter` shows the conversation on a phone; the first room after connecting
// leaves the phone on the room list, like the mock.
async function openRoom(id, enter = true) {
  const room = allRooms().find((candidate) => candidate.room_id === id);
  if (!room) return;
  const unread = unreadOf(room);
  S.room = id;
  S.q = '';
  $('histQ').value = '';
  $('top').classList.remove('searching');
  S.replyTo = null;
  S.edit = false;
  S.profileError = null;
  S.member = null;
  S.receiverEdit = null;
  S.actionError = null;
  S.sendError = null;
  S.infoTab = 'settings';
  resetRoomState();
  prefSet(PREF_ROOM, id);
  if (enter) app.classList.add('in-room');
  if (window.matchMedia('(max-width:1280px)').matches) app.classList.add('info-off');
  renderAll();
  await loadFeed();
  if (S.room !== id) return;
  if (unread > 0 && unread <= S.msgs.length) S.newFrom = S.msgs[S.msgs.length - unread].message_id;
  markSeen(currentRoom());
  renderRooms();
  renderFeed();
  toBottom();
}

async function reloadRoom() {
  resetRoomState();
  renderFeed();
  await loadFeed();
  renderFeed();
  toBottom();
}

function renderAll() {
  renderStyle();
  renderMe();
  renderRooms();
  renderTop();
  renderFeed();
  renderComposer();
  renderInfo();
}

// ---------------------------------------------------------------- session

function readStoredToken() {
  try {
    return window.sessionStorage.getItem(TOKEN_STORAGE_KEY) || '';
  } catch (_) {
    return '';
  }
}

function storeToken(token, keep) {
  try {
    if (keep) window.sessionStorage.setItem(TOKEN_STORAGE_KEY, token);
    else window.sessionStorage.removeItem(TOKEN_STORAGE_KEY);
  } catch (_) {
    // Storage may be unavailable (private mode); the token stays in memory.
  }
}

function loadSeen() {
  try {
    const parsed = JSON.parse(prefGet(PREF_SEEN));
    return parsed && typeof parsed === 'object' ? parsed : null;
  } catch (_) {
    return null;
  }
}

function showGate(message) {
  app.hidden = true;
  $('gate').hidden = false;
  $('loginError').hidden = !message;
  $('loginError').textContent = message || '';
  $('tokenInput').focus();
}

async function connect(token, keep) {
  S.session += 1;
  S.token = token;
  let owner;
  try {
    owner = await request('GET', API.owner());
  } catch (error) {
    const failure = asFailure(error);
    S.token = '';
    storeToken('', false);
    showGate(`연결 실패 — ${failure.status ? `${failure.status} ` : ''}${failureText(failure)}`);
    return;
  }
  storeToken(token, keep);
  S.postAs = owner.post_as;
  S.me = owner.post_as[0] || null;
  $('gate').hidden = true;
  app.hidden = false;
  await refreshDirectory();
  await refreshRooms();
  const seen = loadSeen();
  if (seen) {
    S.seen = seen;
  } else {
    for (const room of S.rooms) S.seen[room.room_id] = room.message_count;
    prefSet(PREF_SEEN, JSON.stringify(S.seen));
  }
  const remembered = prefGet(PREF_ROOM);
  const start = allRooms().some((room) => room.room_id === remembered) ? remembered : 'all';
  await openRoom(start, false);
  stopTimers();
  S.timers.push(window.setInterval(pollFeed, POLL_FEED_MS), window.setInterval(refreshAll, POLL_ROOMS_MS));
}

function stopTimers() {
  for (const timer of S.timers) window.clearInterval(timer);
  S.timers = [];
}

function logout(message) {
  stopTimers();
  S.session += 1;
  S.token = '';
  for (const dialog of document.querySelectorAll('dialog[open]')) dialog.close();
  storeToken('', false);
  for (const id of Array.from(S.avatars.keys())) forgetAvatar(id);
  S.agents = new Map();
  S.directorySignature = '';
  S.rooms = [];
  S.extraRooms = [];
  S.snippets.clear();
  S.quotes.clear();
  S.delivery.clear();
  S.inbox.clear();
  S.room = null;
  S.me = null;
  resetRoomState();
  showGate(message || '');
}

// ----------------------------------------------------------------- events

function onClick(event) {
  const target = event.target.closest('[data-act]');
  if (!target) return;
  const id = target.dataset.id;
  const room = currentRoom();
  switch (target.dataset.act) {
    case 'room':
      openRoom(id);
      break;
    case 'back':
      app.classList.remove('in-room');
      break;
    case 'info':
      app.classList.toggle('info-off');
      renderTop();
      break;
    case 'search':
      $('top').classList.toggle('searching');
      $('histQ').focus();
      break;
    case 'dir':
      renderDir();
      $('dir').showModal();
      $('dir').focus();
      break;
    case 'dir-close':
      $('dir').close();
      break;
    case 'dm-open':
      openDm(id);
      break;
    case 'reg-open':
      openRegister();
      break;
    case 'reg-close':
      $('reg').close();
      break;
    case 'cred-close':
      $('cred').close();
      break;
    case 'copy':
      copySecret(target);
      break;
    case 'del-open':
      openDelete(id);
      break;
    case 'del-close':
      $('del').close();
      break;
    case 'more': {
      const text = target.previousElementSibling;
      const folded = text.classList.toggle('clamp');
      target.textContent = folded ? '더 보기' : '접기';
      if (folded) S.expanded.delete(target.dataset.k);
      else S.expanded.add(target.dataset.k);
      break;
    }
    case 'jump':
      jumpTo(target.dataset.k);
      break;
    case 'reply':
      startReply(target.dataset.k);
      break;
    case 'unreply':
      S.replyTo = null;
      renderComposer();
      break;
    case 'dismiss-send':
      S.sendError = null;
      renderComposer();
      break;
    case 'send':
      send();
      break;
    case 'style':
      S.style = target.dataset.style || (S.style === 'bubble' ? 'flat' : 'bubble');
      prefSet(PREF_STYLE, S.style);
      renderStyle();
      renderFeed();
      toBottom();
      break;
    case 'tab':
      S.tab[room.room_id] = id;
      S.edit = false;
      S.profileError = null;
      S.receiverEdit = null;
      renderInfo();
      break;
    case 'member':
      S.member = id || null;
      S.edit = false;
      S.profileError = null;
      S.receiverEdit = null;
      renderInfo();
      break;
    case 'info-tab':
      S.infoTab = target.dataset.tab;
      if (S.infoTab === 'inbox') S.inbox.delete(infoAgentIdNow());
      renderInfo();
      break;
    case 'edit':
      S.edit = true;
      S.profileError = null;
      renderInfo();
      break;
    case 'edit-cancel':
      S.edit = false;
      S.profileError = null;
      renderInfo();
      break;
    case 'edit-save':
      saveProfile(id);
      break;
    case 'avatar-upload':
      S.avatarTarget = id;
      $('avatarFile').value = '';
      $('avatarFile').click();
      break;
    case 'avatar-clear':
      clearAvatar(id);
      break;
    case 'receiver-edit':
      S.receiverEdit = id;
      renderInfo();
      $('recvInput').focus();
      break;
    case 'receiver-cancel':
      S.receiverEdit = null;
      renderInfo();
      break;
    case 'receiver-save':
      saveReceiver(id);
      break;
    case 'receiver-clear':
      clearReceiver(id);
      break;
    case 'retry':
      retryDelivery(id);
      break;
    case 'join':
      toggleJoin(id);
      break;
    case 'inbox-refresh':
      loadInbox(id, false);
      break;
    case 'inbox-more':
      loadInbox(id, true);
      break;
    case 'logout':
      logout('');
      break;
    default:
      break;
  }
}

let searchTimer = 0;
function onSearch(event) {
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => {
    const query = event.target.value.trim();
    if (query === S.q) return;
    S.q = query;
    reloadRoom();
  }, SEARCH_DEBOUNCE_MS);
}

function onKeydown(event) {
  if (event.key !== 'Escape') return;
  if (S.replyTo) {
    S.replyTo = null;
    renderComposer();
  } else if (S.receiverEdit) {
    S.receiverEdit = null;
    renderInfo();
  }
}

function init() {
  S.style = prefGet(PREF_STYLE) === 'flat' ? 'flat' : 'bubble';
  if (window.matchMedia('(max-width:1280px)').matches) app.classList.add('info-off');
  renderStyle();
  document.addEventListener('click', onClick);
  document.addEventListener('keydown', onKeydown);
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) refreshAll();
  });
  $('roomQ').addEventListener('input', renderRooms);
  $('histQ').addEventListener('input', onSearch);
  $('input').addEventListener('input', grow);
  $('input').addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      send();
    }
  });
  bindSwipe();
  feed.addEventListener('scroll', () => {
    if (feed.scrollTop < OLDER_TRIGGER_PX) loadOlder();
  });
  $('avatarFile').addEventListener('change', (event) => {
    const file = event.target.files[0];
    if (file && S.avatarTarget) uploadAvatar(S.avatarTarget, file);
  });
  $('regForm').addEventListener('submit', (event) => {
    event.preventDefault();
    submitRegister();
  });
  $('delForm').addEventListener('submit', (event) => {
    event.preventDefault();
    confirmDelete();
  });
  $('delInput').addEventListener('input', onDeleteInput);
  $('cred').addEventListener('close', onCredentialsClosed);
  $('loginForm').addEventListener('submit', (event) => {
    event.preventDefault();
    const token = $('tokenInput').value.trim();
    $('tokenInput').value = '';
    if (token) connect(token, $('tokenKeep').checked);
  });
  const stored = readStoredToken();
  if (stored) {
    $('tokenKeep').checked = true;
    connect(stored, true);
  } else {
    showGate('');
  }
}

init();
