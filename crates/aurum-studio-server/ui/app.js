// Aurum Studio's page.
//
// The token arrives in the URL, because opening a browser at a URL is the only
// way to hand it over without a login step. It is moved into sessionStorage and
// stripped from the address bar immediately, so it does not persist in history
// and cannot leak through a Referer header. The page itself never contains the
// token: the server refuses to bake it in.

(function () {
  'use strict';

  const TOKEN_KEY = 'aurum_token';

  // Take the token from the URL once, then get it out of sight.
  const params = new URLSearchParams(window.location.search);
  const fromUrl = params.get('t');
  if (fromUrl) {
    sessionStorage.setItem(TOKEN_KEY, fromUrl);
    params.delete('t');
    const query = params.toString();
    window.history.replaceState(
      {},
      '',
      window.location.pathname + (query ? '?' + query : '')
    );
  }
  const token = sessionStorage.getItem(TOKEN_KEY) || '';

  const $ = (id) => document.getElementById(id);
  const connection = $('connection');
  const connectionText = connection.querySelector('.connection-text');
  const project = $('project');
  const projectPath = $('project-path');
  const log = $('log');
  const health = $('health');
  const busy = $('busy');
  const busyText = busy.querySelector('.status-text');

  const MAX_LINES = 500;
  const EMPTY_LOG = 'Waiting for the first event<span class="caret"></span>';

  // -- small renderers ---------------------------------------------------

  function setState(state, label) {
    connection.dataset.state = state;
    connectionText.textContent = label;
  }

  function setProject(name, root) {
    project.textContent = name || 'project';
    project.title = name || '';
    projectPath.textContent = root || '';
    projectPath.title = root || '';
  }

  function setBusy(text) {
    const active = Boolean(text);
    busy.dataset.active = String(active);
    busyText.textContent = text || '';
  }

  function clearEmptyLog() {
    const placeholder = log.querySelector('.log-empty');
    if (placeholder) placeholder.remove();
  }

  function say(text, kind) {
    clearEmptyLog();

    const line = document.createElement('li');
    if (kind) line.className = kind;

    const time = document.createElement('time');
    time.textContent = new Date().toLocaleTimeString([], { hour12: false });

    const body = document.createElement('span');
    body.textContent = text;

    line.append(time, body);

    // Follow the tail only when the reader is already at the bottom, so a
    // line arriving never yanks the view away from something being read.
    const atBottom = log.scrollTop + log.clientHeight >= log.scrollHeight - 28;
    log.append(line);
    while (log.childElementCount > MAX_LINES) log.removeChild(log.firstChild);
    if (atBottom) log.scrollTop = log.scrollHeight;
  }

  // One finding as the core reports it: what was checked, what was found, and
  // what to do about it. Every value is set as text and never as markup —
  // evidence is a path or a tool's own output, and none of it is ours to trust.
  function findingRow(finding) {
    const row = document.createElement('li');
    // The class is the label the core emits, so the stylesheet has to name the
    // same string. A test in assets.rs holds the two together.
    row.className = 'finding ' + finding.health;

    const head = document.createElement('div');
    head.className = 'finding-head';

    const id = document.createElement('code');
    id.className = 'finding-id';
    id.textContent = finding.id;

    const summary = document.createElement('span');
    summary.className = 'finding-summary';
    summary.textContent = finding.summary;

    head.append(id, summary);
    row.append(head);

    if (finding.evidence) {
      const evidence = document.createElement('div');
      evidence.className = 'finding-evidence';
      evidence.textContent = finding.evidence;
      row.append(evidence);
    }

    // The remedy is the one line here somebody can act on, so it is marked as
    // an instruction rather than left to read as more detail.
    if (finding.remedy) {
      const remedy = document.createElement('div');
      remedy.className = 'finding-remedy';
      remedy.textContent = finding.remedy;
      row.append(remedy);
    }

    return row;
  }

  function renderHealth(event) {
    health.classList.remove('empty');
    health.replaceChildren();

    const verdict = document.createElement('div');
    verdict.className = 'verdict ' + event.verdict;
    verdict.textContent = event.verdict;

    const summary = document.createElement('div');
    summary.className = 'summary';
    summary.textContent = event.summary;

    health.append(verdict, summary);

    const findings = Array.isArray(event.findings) ? event.findings : [];
    if (!findings.length) return;

    // Everything below the verdict shares one bounded, scrolling area, so a
    // long list of problems and an expanded list of healthy checks cannot
    // between them push the console off the bottom of the window.
    const diagnostics = document.createElement('div');
    diagnostics.className = 'diagnostics';

    // Problems first, worst first. That order comes from the core, which is
    // where the levels are ranked, and it is the order that matters: a blocked
    // finding below a stack of green lines is one nobody reads.
    const problems = findings.filter((finding) => finding.health !== 'ok');
    if (problems.length) {
      const list = document.createElement('ul');
      list.className = 'findings';
      for (const problem of problems) list.append(findingRow(problem));
      diagnostics.append(list);
    }

    // The healthy checks are kept, because "eleven checks ran" is a different
    // statement from "nothing was found", and folded away, because eleven
    // lines of reassurance would push the one that matters off the panel.
    const healthy = findings.filter((finding) => finding.health === 'ok');
    if (healthy.length) {
      const quiet = document.createElement('details');
      quiet.className = 'healthy';

      const toggle = document.createElement('summary');
      toggle.className = 'healthy-toggle';
      toggle.textContent =
        healthy.length === 1 ? '1 check passed' : healthy.length + ' checks passed';

      const list = document.createElement('ul');
      list.className = 'findings';
      for (const check of healthy) list.append(findingRow(check));

      quiet.append(toggle, list);
      diagnostics.append(quiet);
    }

    health.append(diagnostics);
  }

  // -- requests ----------------------------------------------------------

  // The page load plants an HttpOnly session cookie, which is what the
  // browser attaches to this page's own asset requests. The copy kept here
  // exists for the API calls, and is sent only when there is one: an empty
  // header would shadow the cookie that is doing the real work.
  async function post(path, body) {
    const headers = { 'Content-Type': 'application/json' };
    if (token) headers['X-Aurum-Token'] = token;
    return fetch(path, {
      method: 'POST',
      headers,
      body: JSON.stringify(body || {}),
    });
  }

  document.querySelectorAll('button[data-command]').forEach((button) => {
    button.addEventListener('click', async () => {
      const command = button.dataset.command;
      const extra = button.dataset.args ? JSON.parse(button.dataset.args) : {};
      setBusy(command + ' requested');
      try {
        const response = await post('/api/command', { command, ...extra });
        if (!response.ok) {
          say((await response.text()).trim(), 'bad');
          setBusy('');
        }
      } catch (error) {
        say('could not reach Studio: ' + error, 'bad');
        setBusy('');
      }
    });
  });

  $('clear').addEventListener('click', () => {
    log.replaceChildren();
    const placeholder = document.createElement('li');
    placeholder.className = 'log-empty';
    // A constant defined here, never anything from the server.
    placeholder.innerHTML = EMPTY_LOG;
    log.append(placeholder);
  });

  $('shutdown').addEventListener('click', async () => {
    setState('closed', 'stopped');
    try {
      await post('/api/stop');
    } catch (error) {
      /* The server may close the connection before it can answer. */
    }
    say('Studio has been asked to shut down. This page can be closed.', 'note');
  });

  // -- events ------------------------------------------------------------

  function handle(event) {
    switch (event.kind) {
      case 'health':
        setBusy('');
        renderHealth(event);
        say('health: ' + event.verdict + ': ' + event.summary);
        break;
      case 'build-started':
        setBusy('building ' + event.package + ' (' + event.profile + ')');
        say('building ' + event.package + ' (' + event.profile + ')', 'note');
        break;
      case 'build-finished':
        setBusy('');
        say(
          (event.ok ? 'build ok: ' : 'build failed: ') + event.summary,
          event.ok ? 'good' : 'bad'
        );
        break;
      case 'process-started':
        setBusy('');
        say(event.process + ' started (pid ' + event.pid + ')', 'good');
        break;
      case 'process-stopped':
        setBusy('');
        say(event.process + ': ' + event.description);
        break;
      case 'change':
        say(event.verdict + ': ' + event.reason);
        break;
      case 'log':
        setBusy('');
        say(event.line);
        break;
      case 'error':
        setBusy('');
        say(event.message, 'bad');
        break;
      case 'stopped':
        setState('closed', 'stopped');
        say('Studio stopped.', 'note');
        break;
      default:
        say(JSON.stringify(event));
    }
  }

  function connect() {
    setState('connecting', 'connecting');
    // EventSource cannot set a header, so the token goes in the query when
    // there is one. When there is not, the session cookie carries it.
    const query = token ? '?t=' + encodeURIComponent(token) : '';
    const stream = new EventSource('/api/events' + query);

    stream.onopen = () => setState('open', 'connected');
    stream.onerror = () => {
      // EventSource retries by itself; report it rather than giving up.
      setState('connecting', 'reconnecting');
    };
    stream.onmessage = (message) => {
      let event;
      try {
        event = JSON.parse(message.data);
      } catch (error) {
        return;
      }
      handle(event);
    };
  }

  // A snapshot first, so the page has something to say before the first event
  // arrives rather than showing an empty shell.
  const stateHeaders = token ? { 'X-Aurum-Token': token } : {};
  fetch('/api/state', { headers: stateHeaders })
    .then((response) => response.json())
    .then((state) => {
      setProject(state.project, state.root);
      say('Studio ' + state.studio + ' · ' + state.root, 'note');
    })
    .catch(() => {
      setProject('');
      say(
        'Not authorised. Open the address ' +
          'aurum studio printed, which carries a session token.',
        'bad'
      );
    })
    .finally(connect);
})();
