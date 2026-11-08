// push 管理页。没有构建步骤；所有来自文档和用户的文字一律用 textContent 插入。

const $ = (sel) => document.querySelector(sel);

/** 创建元素：h('li', { class: 'x' }, '文字', child) */
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, '');
    else el.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : String(c));
  }
  return el;
}

async function api(path, { method = 'GET', body } = {}) {
  const init = { method, headers: {} };
  if (body !== undefined) {
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  if (method !== 'GET') init.headers['X-Push-Admin'] = '1';
  const resp = await fetch(path, init);
  if (!resp.ok) {
    let msg = `${resp.status} ${resp.statusText}`;
    try {
      msg = (await resp.json()).error || msg;
    } catch {}
    throw new Error(msg);
  }
  return resp;
}

const TZ = 'Asia/Shanghai';
const time = (iso) => new Date(iso).toLocaleString('zh-CN', { timeZone: TZ, month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' });
const today = () => new Date().toLocaleDateString('sv-SE', { timeZone: TZ });

function ago(iso) {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return '刚刚';
  if (s < 3600) return `${Math.floor(s / 60)} 分钟前`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小时前`;
  return `${Math.floor(s / 86400)} 天前`;
}

let toastTimer;
function toast(msg, kind = '') {
  const el = $('#toast');
  el.textContent = msg;
  el.className = `show ${kind}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.className = ''), kind === 'error' ? 6000 : 2500);
}
const fail = (err) => toast(err.message, 'error');

// ---------- 状态与活动 ----------

async function loadStatus() {
  const s = await (await api('/api/status')).json();
  const g = s.gateway;
  $('#state').className = `state ${g.online ? 'on' : 'off'}`;
  $('#state-text').textContent = g.online
    ? (ago(g.connected_at) === '刚刚' ? '在线 · 刚连上' : `在线 · 已连接 ${ago(g.connected_at).replace('前', '')}`)
    : `离线${g.last_error ? ` · ${g.last_error}` : ''}`;
  const d = s.daily;
  $('#daily-line').textContent = d
    ? `定时：每天 ${d.at} 把过去 24 小时的战报发到 ${s.subscribers} 个勾选了「每天定时发」的会话，下一次 ${time(d.next)}`
    : '定时战报已关闭';
  const opt = form.mode.querySelector('option[value="daily"]');
  opt.disabled = !d;
  opt.textContent = d ? `下一次定时战报（${time(d.next)}）` : '定时战报已关闭';
  if (!d && daily()) {
    form.mode.value = 'dates';
    syncMode();
  }
  const events = s.events.slice(0, 20);
  $('#log').replaceChildren(
    ...(events.length
      ? events.map((e) => h('li', { class: e.level }, h('time', {}, time(e.at)), h('span', { class: 'msg' }, e.message)))
      : [h('li', {}, '还没有活动')]),
  );
}

// ---------- 战报 ----------

let targets = [];

function targetName(t) {
  const kind = t.kind === 'group' ? '群' : '私聊';
  return t.label ? `${t.label}（${kind}）` : `${kind} ${t.id.split(':')[1].slice(0, 6)}`;
}

async function loadTargets() {
  targets = await (await api('/api/targets')).json();
  const select = $('#target');
  const prev = select.value;
  select.replaceChildren(
    ...targets.map((t) => h('option', { value: t.id }, targetName(t))),
  );
  if (targets.some((t) => t.id === prev)) select.value = prev;
  $('#send-controls').hidden = !targets.length;
  $('#no-targets').hidden = !!targets.length;
  syncDailyToggle();
}

function syncDailyToggle() {
  const t = targets.find((x) => x.id === $('#target').value);
  $('#daily-toggle').checked = !!t?.subscribed;
}

$('#target').addEventListener('change', syncDailyToggle);
$('#daily-toggle').addEventListener('change', async (e) => {
  const t = targets.find((x) => x.id === $('#target').value);
  if (!t) return;
  try {
    await api('/api/targets', { method: 'PATCH', body: { id: t.id, subscribed: e.target.checked } });
    toast(e.target.checked ? `${targetName(t)} 每天定时接收战报` : `${targetName(t)} 不再定时接收`);
    await Promise.all([loadTargets(), loadStatus()]);
  } catch (err) {
    e.target.checked = !e.target.checked;
    fail(err);
  }
});

const form = $('#report');
// 默认预览下一次定时战报，窗口由服务端按本地时间和 DAILY_PUSH_AT 算；空对象就表示这个
const daily = () => form.mode.value === 'daily';
const range = () => (daily() ? {} : { from: form.from.value, to: form.to.value });
const rangeText = () => (daily() ? '下一次定时战报（截至现在的内容）' : `${form.from.value} 至 ${form.to.value} 的战报`);

function syncMode() {
  $('#dates').hidden = daily();
  form.from.required = form.to.required = !daily();
}
form.mode.addEventListener('change', syncMode);
const note = (text) => ($('#report-note').textContent = text);

let previewUrl;
form.addEventListener('submit', async (e) => {
  e.preventDefault();
  const r = range();
  const btn = form.querySelector('button[type="submit"]');
  btn.disabled = true;
  const t0 = performance.now();
  const tick = () => note(`生成中… ${Math.floor((performance.now() - t0) / 1000)} 秒`);
  tick();
  const timer = setInterval(tick, 1000);
  try {
    const blob = await (await api('/api/report/preview', { method: 'POST', body: r })).blob();
    if (previewUrl) URL.revokeObjectURL(previewUrl);
    previewUrl = URL.createObjectURL(blob);
    $('#preview-img').src = previewUrl;
    $('#preview').hidden = false;
    note(`${(blob.size / 1024 / 1024).toFixed(1)} MB · ${((performance.now() - t0) / 1000).toFixed(1)} 秒`);
  } catch (err) {
    note('');
    fail(err);
  } finally {
    clearInterval(timer);
    btn.disabled = false;
    // 预览前会自动加新成员，刷新名单和动态
    loadMembers().catch(() => {});
    loadStatus().catch(() => {});
  }
});

$('#send').addEventListener('click', async (e) => {
  const t = targets.find((x) => x.id === $('#target').value);
  if (!t) return;
  const r = range();
  if (!confirm(`把${rangeText()}发到「${targetName(t)}」？\n以主动消息发送，群里需要给机器人开「主动推送消息」权限。`)) return;
  e.target.disabled = true;
  note('发送中…');
  try {
    await api('/api/report/send', { method: 'POST', body: { ...r, target: t.id } });
    note('');
    toast(`已发到 ${targetName(t)}`);
  } catch (err) {
    note('');
    fail(err);
  } finally {
    e.target.disabled = false;
    loadMembers().catch(() => {});
    loadStatus().catch(() => {});
  }
});

$('#rename').addEventListener('click', async () => {
  const t = targets.find((x) => x.id === $('#target').value);
  if (!t) return;
  const label = prompt(`给 ${t.id} 起个备注（QQ 不提供群名）`, t.label || '');
  if (label === null) return;
  try {
    await api('/api/targets', { method: 'PATCH', body: { id: t.id, label } });
    await loadTargets();
  } catch (err) {
    fail(err);
  }
});

// ---------- 成员 ----------

async function patchMember(body) {
  try {
    await api('/api/members', { method: 'PATCH', body });
    toast('已保存');
    await loadMembers();
  } catch (err) {
    fail(err);
  }
}

async function removeMember(m, name) {
  if (!confirm(`从名单里移除「${name}」？`)) return;
  try {
    const { ignored } = await (await api('/api/members', { method: 'DELETE', body: { doc: m.doc } })).json();
    toast(ignored ? '这篇文档会被自动同步加回来，已改为「忽略」' : '已移除');
    await loadMembers();
  } catch (err) {
    fail(err);
  }
}

function memberItem(m) {
  const r = m.resolved;
  const shown = r?.display_name || m.alias || m.title || m.doc;
  const name = r?.display_name
    ? h('b', {}, r.display_name)
    : h('b', {}, m.alias || m.title || '（未知）', h('span', { class: 'tag' }, r ? '缺名，不统计' : '读取失败'));
  const sub = r
    ? h('span', { class: 'sub' }, h('a', { href: r.url, target: '_blank', rel: 'noopener noreferrer' }, r.title), r.account_name ? ` · 账号 ${r.account_name}` : ' · 账号名像真名，已隐藏', ` · ${ago(r.updated_at)}改过`)
    : h('span', { class: 'sub' }, m.error || m.doc);
  return h('li', { class: m.ignore ? 'ignored' : '' },
    h('div', { class: 'who' }, name, sub),
    h('input', {
      class: 'alias', type: 'text', value: m.alias || '', placeholder: '昵称', 'aria-label': `${shown} 的昵称`,
      title: '留空则用默认显示名，回车保存',
      onchange: (e) => patchMember({ doc: m.doc, alias: e.target.value }),
    }),
    h('label', { class: 'toggle' },
      h('input', { type: 'checkbox', checked: m.ignore, onchange: (e) => patchMember({ doc: m.doc, ignore: e.target.checked }) }),
      '忽略'),
    h('button', { type: 'button', class: 'quiet', title: '从名单移除', 'aria-label': `移除 ${shown}`, onclick: () => removeMember(m, shown) }, '移除'));
}

async function loadMembers() {
  const list = await (await api('/api/members')).json();
  const active = list.filter((m) => !m.ignore).length;
  const missing = list.filter((m) => !m.ignore && m.resolved && !m.resolved.display_name).length;
  $('#members-count').textContent = list.length ? `${active} 人${missing ? ` · ${missing} 人缺名` : ''}` : '';
  $('#members').replaceChildren(
    ...(list.length
      ? list.map(memberItem)
      : [h('li', { class: 'empty' }, '还没有成员。有人在 pushpush 新建进度文档后，下次 push 时会自动加进来。')]),
  );
}

$('#add-member').addEventListener('submit', async (e) => {
  e.preventDefault();
  const input = e.target.doc;
  const btn = e.target.querySelector('button');
  btn.disabled = true;
  try {
    const { title } = await (await api('/api/members', { method: 'POST', body: { doc: input.value.trim() } })).json();
    input.value = '';
    toast(`已添加「${title}」`);
    await loadMembers();
  } catch (err) {
    fail(err);
  } finally {
    btn.disabled = false;
  }
});

$('#sync').addEventListener('click', async (e) => {
  e.target.disabled = true;
  try {
    const { added } = await (await api('/api/members/sync', { method: 'POST' })).json();
    toast(added ? `新加入 ${added} 人` : '没有新成员');
    await loadMembers();
  } catch (err) {
    fail(err);
  } finally {
    e.target.disabled = false;
  }
});

// ---------- 启动 ----------

form.from.value = form.to.value = today();
syncMode();
Promise.all([loadStatus(), loadTargets(), loadMembers()]).catch(fail);
setInterval(() => {
  if (!document.hidden) loadStatus().catch(() => {});
}, 15000);
