// The Heist: front end. Plain ES modules, no build step.
// Practice mode runs the same odds in the browser; on-chain mode plays the NEAR testnet contract.

const $ = id => document.getElementById(id);
const DECIMALS = 18n;
const ONE = 10n ** DECIMALS;
const EDGE = 0.03;

// ---------- deployment & chain reads ----------
const deployment = await fetch('deployment.json', { cache: 'no-store' }).then(r => r.json()).catch(() => null);
const RPC = deployment?.rpc || 'https://test.rpc.fastnear.com';
const EXPLORER = 'https://testnet.nearblocks.io';

async function view(contract, method, args = {}) {
  const res = await fetch(RPC, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 'heist', method: 'query', params: { request_type: 'call_function', finality: 'optimistic', account_id: contract, method_name: method, args_base64: btoa(JSON.stringify(args)) } }),
  }).then(r => r.json());
  if (res.error || res.result?.error) throw new Error(res.error?.message || res.result.error);
  return JSON.parse(new TextDecoder().decode(new Uint8Array(res.result.result)));
}

const toUnits = coins => BigInt(Math.round(coins * 100)) * ONE / 100n;
const fromUnits = (raw, digits = 2) => {
  const v = BigInt(raw);
  const whole = v / ONE;
  const frac = Number((v % ONE) * 100n / ONE) / 100;
  const n = Number(whole) + frac;
  return n.toLocaleString('en-US', { maximumFractionDigits: digits });
};
const fmt = n => Number(n).toLocaleString('en-US', { maximumFractionDigits: 2 });
const short = a => a.length > 22 ? a.slice(0, 10) + '…' + a.slice(-8) : a;

// ---------- state ----------
const state = {
  mode: 'practice',
  practice: Number(localStorageGet('heist:practice') ?? 1000),
  account: null,
  wallet: null,
  chainBalance: null,
  maxBetRaw: null,
  running: false,
  auto: false,
};
function localStorageGet(k) { try { return localStorage.getItem(k); } catch { return null; } }
function localStorageSet(k, v) { try { localStorage.setItem(k, v); } catch { /* private mode */ } }

// ---------- odds & form ----------
const betInput = $('bet'), escInput = $('escape');
function readForm() {
  const bet = Math.max(0, Number(betInput.value) || 0);
  let escape = Number(escInput.value) || 0;
  return { bet, escape, escapeX100: Math.round(escape * 100) };
}
function updateOdds() {
  const { bet, escape, escapeX100 } = readForm();
  const valid = escapeX100 >= 101 && escapeX100 <= 100000;
  $('chance').textContent = valid ? ((1 - EDGE) / (escapeX100 / 100) * 100).toFixed(2) + '%' : '–';
  $('win').textContent = valid ? fmt(bet * escapeX100 / 100) : '–';
  $('burn').textContent = fmt(bet);
  for (const b of document.querySelectorAll('#quick button')) b.classList.toggle('on', Number(b.dataset.x) === escape);
  void refreshMaxBet();
}
betInput.addEventListener('input', updateOdds);
escInput.addEventListener('input', updateOdds);
$('quick').addEventListener('click', e => { const x = e.target.dataset?.x; if (x) { escInput.value = Number(x).toFixed(2); updateOdds(); } });
document.querySelectorAll('[data-bet]').forEach(b => b.addEventListener('click', () => {
  const v = Number(betInput.value) || 1;
  betInput.value = String(b.dataset.bet === 'half' ? Math.max(1, Math.floor(v / 2)) : v * 2);
  updateOdds();
}));

let maxBetTimer;
async function refreshMaxBet() {
  if (state.mode !== 'chain' || !deployment) return;
  clearTimeout(maxBetTimer);
  maxBetTimer = setTimeout(async () => {
    const { escapeX100 } = readForm();
    if (escapeX100 < 101) return;
    try { state.maxBetRaw = BigInt(await view(deployment.heist, 'max_bet', { escape: escapeX100 })); } catch { state.maxBetRaw = null; }
  }, 250);
}

// ---------- mode & balance ----------
function setMode(mode) {
  state.mode = mode;
  $('mode-practice').setAttribute('aria-selected', String(mode === 'practice'));
  $('mode-chain').setAttribute('aria-selected', String(mode === 'chain'));
  note('');
  renderBalance();
  updateOdds();
}
$('mode-practice').addEventListener('click', () => setMode('practice'));
$('mode-chain').addEventListener('click', () => setMode('chain'));

function renderBalance() {
  const faucet = $('faucet');
  if (state.mode === 'practice') {
    $('balance').textContent = fmt(state.practice);
    faucet.hidden = state.practice >= 1;
    faucet.textContent = 'Refill 1,000 practice coins';
    $('go').textContent = 'ROB THE VAULT';
  } else {
    $('balance').textContent = state.account ? (state.chainBalance === null ? '…' : fromUnits(state.chainBalance) + ' tGOLD') : 'connect';
    faucet.hidden = !state.account;
    faucet.textContent = 'Get 10,000 free tGOLD';
    $('go').textContent = state.account ? 'ROB THE VAULT · ON-CHAIN' : 'CONNECT TO PLAY ON-CHAIN';
  }
}
async function refreshChainBalance() {
  if (!state.account || !deployment) return;
  try { state.chainBalance = await view(deployment.token, 'ft_balance_of', { account_id: state.account }); } catch { /* keep */ }
  renderBalance();
}

$('faucet').addEventListener('click', async () => {
  if (state.mode === 'practice') { state.practice = 1000; localStorageSet('heist:practice', '1000'); renderBalance(); return; }
  try {
    note('Approve the free tGOLD in your wallet…');
    await sendTx(deployment.token, 'faucet', {}, '0', '30000000000000');
    note('10,000 tGOLD received.');
    await refreshChainBalance();
  } catch (e) { note(e.message || 'Faucet failed', true); }
});

function note(text, error = false) { const n = $('note'); n.textContent = text; n.classList.toggle('err', error); }

// ---------- wallet (NEAR testnet, via @hot-labs/near-connect) ----------
let connector;
async function getConnector() {
  if (connector) return connector;
  const { NearConnector } = await import('./vendor/near-connect.js?v=' + (import.meta.url.split('?v=')[1] || '1'));
  connector = new NearConnector({ network: 'testnet', autoConnect: true, excludedWallets: ['ethereum-wallets', 'near-cli'] });
  connector.on('wallet:signOut', () => { state.account = null; state.wallet = null; renderWallet(); });
  return connector;
}
async function connect() {
  const c = await getConnector();
  const wallet = await c.connect();
  const [acc] = await wallet.getAccounts({ network: 'testnet' });
  if (!acc) throw new Error('No account shared');
  state.wallet = wallet; state.account = acc.accountId;
  renderWallet();
}
async function restore() {
  try {
    const c = await getConnector();
    const wallet = await c.wallet().catch(() => null);
    if (!wallet) return;
    const [acc] = await wallet.getAccounts({ network: 'testnet' });
    if (acc) { state.wallet = wallet; state.account = acc.accountId; renderWallet(); }
  } catch { /* no wallet yet */ }
}
function renderWallet() {
  $('connect').textContent = state.account ? short(state.account) : 'Connect wallet';
  if (state.account) { setMode('chain'); void refreshChainBalance(); } else renderBalance();
}
$('connect').addEventListener('click', async () => {
  try {
    if (state.account) { await connector?.disconnect(); state.account = null; state.wallet = null; renderWallet(); return; }
    note('Opening your wallet…');
    await connect();
    note('');
  } catch (e) { note(e?.message || 'Wallet connection failed', true); }
});
async function sendTx(receiverId, methodName, args, deposit, gas) {
  if (!state.wallet) throw new Error('Connect your wallet first');
  return state.wallet.signAndSendTransaction({
    network: 'testnet', signerId: state.account, receiverId,
    actions: [{ type: 'FunctionCall', params: { methodName, args, gas, deposit } }],
  });
}

// ---------- the heist animation ----------
const canvas = $('canvas'), ctx = canvas.getContext('2d');
const foxImg = Object.assign(new Image(), { src: 'assets/fox-badge.png' });
const sheriffImg = Object.assign(new Image(), { src: 'assets/sheriff-badge.png' });
const multAt = t => Math.exp(0.28 * t + 0.012 * t * t);
const timeFor = m => { const a = 0.012, b = 0.28, c = -Math.log(m); return (-b + Math.sqrt(b * b - 4 * a * c)) / (2 * a); };

function size() {
  const r = canvas.getBoundingClientRect();
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  canvas.width = Math.round(r.width * dpr); canvas.height = Math.round(r.height * dpr);
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { w: r.width, h: r.height };
}

function draw(t, { escape, caught, crash }) {
  const { w, h } = size();
  ctx.clearRect(0, 0, w, h);
  const m = multAt(t);
  const pad = { l: 44, r: 24, t: 120, b: 30 };
  const tMax = Math.max(6, t * 1.15);
  const mMax = Math.max(2.2, m * 1.25, escape <= 12 ? escape * 1.15 : 0);
  const X = s => pad.l + (s / tMax) * (w - pad.l - pad.r);
  const Y = v => h - pad.b - ((v - 1) / (mMax - 1)) * (h - pad.t - pad.b);

  // grid
  ctx.strokeStyle = '#c9a46422'; ctx.lineWidth = 1; ctx.fillStyle = '#a99c84'; ctx.font = '11px Oswald, sans-serif';
  const steps = niceSteps(mMax);
  for (const v of steps) { ctx.beginPath(); ctx.moveTo(pad.l, Y(v)); ctx.lineTo(w - pad.r, Y(v)); ctx.stroke(); ctx.fillText(v + '×', 6, Y(v) + 4); }

  // escape line
  if (escape <= mMax) {
    ctx.setLineDash([6, 6]); ctx.strokeStyle = '#6fbf73aa'; ctx.lineWidth = 1.5;
    ctx.beginPath(); ctx.moveTo(pad.l, Y(escape)); ctx.lineTo(w - pad.r, Y(escape)); ctx.stroke(); ctx.setLineDash([]);
    ctx.fillStyle = '#6fbf73'; ctx.fillText('LOOT AT ' + escape.toFixed(2) + '×', w - pad.r - 130, Y(escape) - 6);
  }

  // the arrow's flight: a glowing trail from Robin's bow
  const n = 80;
  ctx.lineCap = 'round';
  for (let i = 1; i <= n; i++) {
    const s0 = (t * (i - 1)) / n, s1 = (t * i) / n;
    ctx.strokeStyle = caught ? `rgba(208,69,58,${0.15 + 0.6 * i / n})` : `rgba(240,217,160,${0.08 + 0.7 * i / n})`;
    ctx.lineWidth = 1 + 3 * i / n;
    ctx.beginPath(); ctx.moveTo(X(s0), Y(multAt(s0))); ctx.lineTo(X(s1), Y(multAt(s1))); ctx.stroke();
  }

  // Robin at the bow, bottom left
  const r = 24;
  if (foxImg.complete) ctx.drawImage(foxImg, X(0) - r - 6, Y(1) - r * 2 + 6, r * 2, r * 2);

  // the arrow at the tip, pointing along its flight
  const x = X(t), y = Y(m);
  const dt = 0.05, angle = Math.atan2(Y(multAt(t + dt)) - y, X(t + dt) - x);
  drawArrow(x, y, caught ? angle + 0.9 : angle, caught);
  if (!caught) for (let k = 0; k < 6; k++) { // sparks
    ctx.fillStyle = `rgba(240,217,160,${Math.random() * 0.8})`;
    ctx.fillRect(x - Math.cos(angle) * (20 + Math.random() * 30) + (Math.random() - .5) * 10, y - Math.sin(angle) * (20 + Math.random() * 30) + (Math.random() - .5) * 10, 2, 2);
  }
  if (caught && sheriffImg.complete) { const R = 30; ctx.drawImage(sheriffImg, x - R + 18, y - R - 26, R * 2, R * 2); }
}
function drawArrow(x, y, angle, broken) {
  ctx.save(); ctx.translate(x, y); ctx.rotate(angle);
  ctx.strokeStyle = broken ? '#8a3a32' : '#c9a464'; ctx.lineWidth = 3;
  ctx.beginPath(); ctx.moveTo(-46, 0); ctx.lineTo(broken ? -12 : 0, 0); ctx.stroke(); // shaft (snapped when caught)
  if (!broken) { ctx.fillStyle = '#f0d9a0'; ctx.beginPath(); ctx.moveTo(12, 0); ctx.lineTo(-2, -6); ctx.lineTo(-2, 6); ctx.closePath(); ctx.fill(); } // head
  ctx.fillStyle = broken ? '#8a3a32' : '#d0453a'; // red fletching
  for (const side of [-1, 1]) { ctx.beginPath(); ctx.moveTo(-46, 0); ctx.lineTo(-56, side * 8); ctx.lineTo(-40, 0); ctx.closePath(); ctx.fill(); }
  ctx.restore();
}
function niceSteps(max) {
  const raw = (max - 1) / 4; const p = 10 ** Math.floor(Math.log10(raw)); const s = [1, 2, 5, 10].map(k => k * p).find(k => k >= raw) || raw;
  const out = []; for (let v = 1; v <= max + 1e-9; v += s) out.push(Number(v.toFixed(2))); return out;
}

function animate({ crash, escape }) {
  return new Promise(resolve => {
    const stage = $('stage'), mult = $('mult'), verdict = $('verdict');
    mult.className = 'mult'; verdict.textContent = 'Robin looses the arrow…';
    const tCrash = timeFor(crash), tEscape = timeFor(escape);
    const won = crash >= escape;
    let escapedShown = false;
    const start = performance.now();
    function frame(now) {
      // After an escape, fast-forward to show where the Sheriff would have struck.
      let t = (now - start) / 1000;
      if (won && t > tEscape + 0.6) t = tEscape + 0.6 + (t - tEscape - 0.6) * 4;
      const done = t >= tCrash;
      if (done) t = tCrash;
      const m = done ? crash : multAt(t);
      mult.textContent = m.toFixed(2) + '×';
      if (won && !escapedShown && m >= escape) {
        escapedShown = true; play('sfx-coins');
        mult.classList.add('escaped'); verdict.textContent = 'The arrow hit the loot at ' + escape.toFixed(2) + '×!';
        stage.classList.remove('flash-gold'); void stage.offsetWidth; stage.classList.add('flash-gold');
      }
      draw(t, { escape, caught: done, crash });
      if (!done) return requestAnimationFrame(frame);
      play('sfx-hit');
      if (!won) {
        mult.classList.add('caught'); verdict.textContent = 'The Sheriff snapped the arrow at ' + crash.toFixed(2) + '×';
        stage.classList.remove('flash-red'); void stage.offsetWidth; stage.classList.add('flash-red');
      } else {
        verdict.textContent = 'Loot taken at ' + escape.toFixed(2) + '× · the Sheriff snapped it at ' + crash.toFixed(2) + '×';
      }
      resolve(won);
    }
    requestAnimationFrame(frame);
  });
}
function play(id) { const a = $(id); try { a.currentTime = 0; void a.play().catch(() => {}); } catch { /* muted */ } }

// The same formula as the contract: crash(x100) = floor(97 / (1 - u)), at least 1.00x.
function practiceCrash() {
  const buf = new Uint32Array(2); crypto.getRandomValues(buf);
  const u = (buf[0] * 2 ** 21 + (buf[1] >>> 11)) / 2 ** 53;
  return Math.min(10000, Math.max(100, Math.floor((100 - EDGE * 100) / (1 - u)))) / 100;
}

// ---------- live flight: the arrow flies until you tap RUN ----------
let runRequest = null; // set while an arrow is flying: call it to run
function fly(startEpochMs, skewMs, planned) {
  return new Promise(resolve => {
    const stage = $('stage'), mult = $('mult'), verdict = $('verdict'), go = $('go');
    mult.className = 'mult'; verdict.textContent = 'The arrow is flying… tap RUN!';
    go.disabled = false; go.classList.remove('running'); go.classList.add('flying');
    let done = false;
    const finish = (escapeX100, expired) => {
      if (done) return; done = true; runRequest = null; state.coolUntil = Date.now() + 1500;
      go.classList.remove('flying'); lock(true);
      resolve({ escapeX100, expired });
    };
    runRequest = () => {
      const t = (Date.now() + skewMs - startEpochMs) / 1000;
      finish(Math.max(101, Math.min(100000, Math.floor(multAt(Math.max(0, t)) * 100))), false);
    };
    function frame() {
      if (done) return;
      const t = Math.max(0, (Date.now() + skewMs - startEpochMs) / 1000);
      const m = Math.min(1000, multAt(t));
      mult.textContent = m.toFixed(2) + '×';
      go.textContent = 'RUN! · ' + m.toFixed(2) + '×';
      draw(t, { escape: planned, caught: false });
      if (state.auto && m >= planned) return runRequest();
      if (t * 1000 > 20000) return finish(0, true);
      requestAnimationFrame(frame);
    }
    requestAnimationFrame(frame);
  });
}
// Show the outcome once the dice is rolled: the arrow stops at your multiplier; the Sheriff's point is revealed.
function reveal({ escape, crash, won }) {
  const stage = $('stage'), mult = $('mult'), verdict = $('verdict');
  play('sfx-hit');
  if (won) {
    play('sfx-coins');
    mult.className = 'mult escaped'; mult.textContent = escape.toFixed(2) + '×';
    verdict.textContent = 'You ran at ' + escape.toFixed(2) + '× · the Sheriff would have snapped it at ' + crash.toFixed(2) + '×';
    draw(timeFor(Math.max(escape, 1.0001)), { escape, caught: false });
    stage.classList.remove('flash-gold'); void stage.offsetWidth; stage.classList.add('flash-gold');
  } else {
    mult.className = 'mult caught'; mult.textContent = crash.toFixed(2) + '×';
    verdict.textContent = escape ? 'Too late! The Sheriff snapped the arrow at ' + crash.toFixed(2) + '×' : 'You never ran: the Sheriff took it all';
    draw(timeFor(Math.max(crash, 1.0001)), { escape: escape || 2, caught: true });
    stage.classList.remove('flash-red'); void stage.offsetWidth; stage.classList.add('flash-red');
  }
}
async function chainSkew() {
  // How far the chain's clock is from this device's clock (block time minus local time).
  try {
    const r = await fetch(RPC, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 's', method: 'block', params: { finality: 'optimistic' } }) }).then(r => r.json());
    return Number(BigInt(r.result.header.timestamp_nanosec) / 1000000n) - Date.now();
  } catch { return 0; }
}

// ---------- play ----------
$('auto').addEventListener('change', e => { state.auto = e.target.checked; });
$('go').addEventListener('click', async () => {
  if (runRequest) return runRequest();
  if (state.running || Date.now() < (state.coolUntil || 0)) return;
  const { bet, escape, escapeX100 } = readForm();
  const planned = escapeX100 >= 101 ? escape : 2;
  if (state.auto && (escapeX100 < 101 || escapeX100 > 100000)) return note('Set the green line between 1.01× and 1000×.', true);
  if (bet < 1) return note('The minimum bet is 1 coin.', true);

  if (state.mode === 'practice') {
    if (bet > state.practice) return note('Not enough practice coins.', true);
    state.running = true; note('');
    state.practice -= bet; renderBalance();
    const { escapeX100: ran, expired } = await fly(Date.now(), 0, planned);
    const crash = practiceCrash(); // rolled when you run, like the contract
    const esc = expired ? 0 : ran / 100;
    const won = !expired && crash >= esc;
    reveal({ escape: esc, crash, won });
    if (won) state.practice += bet * ran / 100;
    state.practice = Math.round(state.practice * 100) / 100;
    localStorageSet('heist:practice', String(state.practice));
    renderBalance(); note(won ? `You escaped: +${fmt(bet * ran / 100 - bet)} coins.` : `Caught. ${fmt(bet)} to the Sheriff's vault.`);
    state.running = false; lock(false); $('go').textContent = 'ROB THE VAULT';
    return;
  }

  // on-chain
  if (!deployment) return note('No testnet deployment found.', true);
  if (!state.account) { try { await connect(); } catch (e) { return note(e?.message || 'Wallet connection failed', true); } return; }
  const raw = toUnits(bet);
  if (state.chainBalance !== null && raw > BigInt(state.chainBalance)) return note('Not enough tGOLD: tap "Get 10,000 free tGOLD".', true);
  state.running = true; lock(true);
  try {
    const before = await view(deployment.heist, 'get_stats');
    note('Approve the bet in your wallet…');
    $('verdict').textContent = 'Robin nocks an arrow…';
    await sendTx(deployment.token, 'ft_transfer_call', { receiver_id: deployment.heist, amount: raw.toString(), msg: JSON.stringify({ start: {} }) }, '1', '150000000000000');
    note('');
    let round = null;
    for (let i = 0; i < 20 && !round; i++) { round = await view(deployment.heist, 'get_round', { account: state.account }).catch(() => null); if (!round) await new Promise(r => setTimeout(r, 1000)); }
    if (!round) throw new Error('The bet was not accepted (refunded). Try a smaller bet.');
    const skew = await chainSkew();
    const { escapeX100: ran, expired } = await fly(round.start_ms, skew, planned);
    $('verdict').textContent = expired ? 'Too slow…' : `You ran at ${(ran / 100).toFixed(2)}× · approve in your wallet`;
    if (!expired) await sendTx(deployment.heist, 'cash_out', { escape: ran }, '0', '50000000000000');
    note('NEAR is rolling the Sheriff’s dice…');
    const heist = expired ? null : await findMyHeist(before.heists);
    if (heist) {
      reveal({ escape: heist.escape / 100, crash: heist.crash / 100, won: heist.won });
      note(heist.won ? `You escaped with ${fromUnits(heist.payout.toString())} tGOLD.` : `Caught. ${fromUnits(heist.bet.toString())} tGOLD to the Sheriff's vault.`);
    } else if (expired) {
      reveal({ escape: 0, crash: 1, won: false });
      note('Too slow: the Sheriff wins this one (it is settled when you play again).');
    } else throw new Error('The result is not visible yet. Check the list below in a moment.');
    await Promise.all([refreshChainBalance(), refreshBoard()]);
  } catch (e) {
    runRequest = null;
    note(e?.message || 'The heist did not go through.', true);
    $('verdict').textContent = 'Try again';
  } finally { state.running = false; lock(false); $('go').classList.remove('flying'); renderBalance(); }
});
async function findMyHeist(fromId) {
  for (let i = 0; i < 20; i++) {
    const list = await view(deployment.heist, 'get_recent', { limit: 20 }).catch(() => []);
    const mine = list.filter(h => h.player === state.account && h.id >= fromId && h.escape > 0).sort((a, b) => b.id - a.id)[0];
    if (mine) return mine;
    await new Promise(r => setTimeout(r, 1500));
  }
  return null;
}
function lock(on) { const g = $('go'); g.disabled = on; g.classList.toggle('running', on); }

// ---------- board ----------
async function refreshBoard() {
  if (!deployment) { $('recent').innerHTML = '<li class="muted">No testnet deployment yet.</li>'; return; }
  try {
    const [config, stats, recent] = await Promise.all([view(deployment.heist, 'get_config'), view(deployment.heist, 'get_stats'), view(deployment.heist, 'get_recent', { limit: 15 })]);
    $('s-vault').textContent = fromUnits(config.vault, 0) + ' tGOLD';
    $('s-heists').textContent = fmt(stats.heists);
    $('s-escapes').textContent = stats.heists ? `${fmt(stats.escapes)} (${Math.round(stats.escapes / stats.heists * 100)}%)` : '0';
    $('s-burned').textContent = fromUnits(stats.paid, 0) + ' tGOLD';
    $('recent').innerHTML = recent.length ? recent.map(h => `<li class="${h.player === state.account ? 'mine' : ''}">
      <span class="who" title="${h.player}">#${h.id} · ${short(h.player)}</span>
      <span class="bet">${fromUnits(h.bet)} ${h.escape ? 'ran at ' + (h.escape / 100).toFixed(2) + '×' : 'never ran'}</span>
      <span>snapped ${(h.crash / 100).toFixed(2)}×</span>
      <span class="${h.won ? 'won' : 'lost'}">${h.won ? '+' + fromUnits(h.payout) : '−' + fromUnits(h.bet)}</span></li>`).join('') : '<li class="muted">No heists yet. Be the first.</li>';
  } catch { $('recent').innerHTML = '<li class="muted">Could not reach NEAR testnet right now.</li>'; }
}

// ---------- start ----------
if (deployment) {
  $('contract-link').textContent = deployment.heist; $('contract-link').href = `${EXPLORER}/address/${deployment.heist}`;
  $('token-link').textContent = deployment.token; $('token-link').href = `${EXPLORER}/address/${deployment.token}`;
}
window.addEventListener('resize', () => draw(0, { escape: readForm().escape || 2, caught: false }));
draw(0, { escape: 2, caught: false });
updateOdds(); renderBalance();
void refreshBoard(); setInterval(refreshBoard, 15000);
void restore();
