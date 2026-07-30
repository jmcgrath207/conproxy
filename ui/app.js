(function () {
  'use strict';

  const BASE = '/'; // Status endpoints live at root
  const REFRESH_MS = 3000;
  let currentPanel = 'overview';
  let refreshTimer = null;

  // --- Navigation ---
  document.getElementById('nav').addEventListener('click', function (e) {
    var link = e.target.closest('[data-panel]');
    if (!link) return;
    e.preventDefault();
    var panel = link.dataset.panel;
    if (panel === currentPanel) return;

    document.querySelectorAll('nav a.active').forEach(function (a) { a.classList.remove('active'); });
    link.classList.add('active');
    document.querySelectorAll('.panel.active').forEach(function (p) { p.classList.remove('active'); });
    document.getElementById('panel-' + panel).classList.add('active');
    currentPanel = panel;
    fetchPanel(panel);
  });

  // --- Fetch helpers ---
  function fetchJSON(path) {
    return fetch(BASE + path, { headers: { 'Accept': 'application/json' } })
      .then(function (r) {
        if (!r.ok) throw new Error(r.status + ' ' + r.statusText);
        return r.json();
      });
  }

  function fetchText(path) {
    return fetch(BASE + path)
      .then(function (r) {
        if (!r.ok) throw new Error(r.status + ' ' + r.statusText);
        return r.text();
      });
  }

  // --- Rendering ---
  function renderCards(containerId, items) {
    var el = document.getElementById(containerId);
    if (!el) return;
    var html = '';
    items.forEach(function (item) {
      var cls = item.cls || '';
      html += '<div class="card"><div class="label">' + esc(item.label) + '</div><div class="value ' + cls + '">' + esc(String(item.value)) + '</div></div>';
    });
    el.innerHTML = html;
  }

  function renderTable(tableId, rows) {
    var tbody = document.querySelector('#' + tableId + ' tbody');
    if (!tbody) return;
    var html = '';
    rows.forEach(function (row) {
      html += '<tr>';
      row.forEach(function (cell) { html += '<td>' + esc(String(cell)) + '</td>'; });
      html += '</tr>';
    });
    tbody.innerHTML = html;
  }

  function renderPre(id, data) {
    var el = document.getElementById(id);
    if (!el) return;
    if (data === null || data === undefined) {
      el.textContent = 'Not available';
    } else if (typeof data === 'object') {
      el.textContent = JSON.stringify(data, null, 2);
    } else {
      el.textContent = String(data);
    }
  }

  function esc(s) {
    var d = document.createElement('div');
    d.appendChild(document.createTextNode(s));
    return d.innerHTML;
  }

  // --- Panel fetchers ---
  function fetchOverview() {
    return Promise.all([fetchJSON('metrics'), fetchJSON('stats'), fetchJSON('circuit')])
      .then(function (res) {
        var m = res[0], stats = res[1], circuit = res[2];
        renderCards('overview-cards', [
          { label: 'Total Requests', value: fmtNum(m.proxy.requests_total) },
          { label: 'Cache Hits', value: fmtNum(m.proxy.cache_hits), cls: 'green' },
          { label: 'Cache Misses', value: fmtNum(m.proxy.cache_misses), cls: 'yellow' },
          { label: 'Error Rate', value: fmtPct(m.proxy.upstream_error_rate), cls: m.proxy.upstream_error_rate > 0.05 ? 'red' : 'green' },
          { label: 'Uptime', value: fmtDuration(stats.uptime_secs) },
          { label: 'Circuit State', value: circuit.state || 'closed', cls: (circuit.state === 'open') ? 'red' : 'green' },
        ]);
      })
      .catch(function () { /* ignore */ });
  }

  function fetchCache() {
    return Promise.all([fetchJSON('stats'), fetchJSON('pool'), fetchJSON('cache/integrity')])
      .then(function (res) {
        var stats = res[0], pool = res[1], integrity = res[2];
        renderCards('cache-cards', [
          { label: 'Cache Size', value: fmtNum(stats.cache.total) },
          { label: 'Max Entries', value: fmtNum(stats.cache.max_entries) },
          { label: 'Fresh', value: fmtNum(stats.cache.fresh), cls: 'green' },
          { label: 'Stale', value: fmtNum(stats.cache.stale), cls: 'yellow' },
          { label: 'Expired', value: fmtNum(stats.cache.expired), cls: 'red' },
        ]);
        if (pool && pool.upstreams) {
          var rows = pool.upstreams.map(function (u) {
            return [u.id, u.status, u.requests, u.failures, fmtPct(u.failure_rate)];
          });
          renderTable('cache-upstreams-table', rows);
        }
        renderPre('cache-integrity', integrity);
      })
      .catch(function () { /* ignore */ });
  }

  function fetchPool() {
    return fetchJSON('pool')
      .then(function (data) { renderPre('pool-data', data); })
      .catch(function () { renderPre('pool-data', null); });
  }

  function fetchCircuitQueue() {
    return Promise.all([fetchJSON('circuit'), fetchJSON('queue')])
      .then(function (res) {
        renderPre('circuit-data', res[0]);
        renderPre('queue-data', res[1]);
      })
      .catch(function () { /* ignore */ });
  }

  function fetchMetrics() {
    return Promise.all([fetchJSON('metrics'), fetchJSON('pool'), fetchJSON('stats/queries')])
      .then(function (res) {
        var m = res[0], pool = res[1], queries = res[2];
        renderCards('metrics-cards', [
          { label: 'Total Requests', value: fmtNum(m.proxy.requests_total) },
          { label: 'Avg Latency', value: fmtMs(m.proxy.latency_avg_ms) },
          { label: 'P99 Latency', value: fmtMs(m.latency_percentiles.p99_ms) },
          { label: 'Active Connections', value: fmtNum(pool.stats.active_connections) },
          { label: 'Cache Hit Rate', value: fmtPct(m.proxy.cache_hit_rate) },
          { label: 'Upstream Errors', value: fmtNum(m.proxy.upstream_failures) },
        ]);
        if (queries && queries.hot_queries && queries.hot_queries.length > 0) {
          var rows = queries.hot_queries.map(function (q) {
            return [q.query || '-', fmtNum(q.count), fmtNum(q.cache_hits), fmtNum(q.cache_misses)];
          });
          renderTable('query-stats-table', rows);
        }
      })
      .catch(function () { /* ignore */ });
  }

  function fetchContexts() {
    return Promise.all([fetchJSON('contexts/current'), fetchJSON('contexts')])
      .then(function (res) {
        var current = res[0], list = res[1];
        renderPre('current-context', current);
        renderPre('contexts-list', list);
      })
      .catch(function () { /* ignore */ });
  }

  function fetchPeer() {
    return fetchText('peer/status')
      .then(function (t) { renderPre('peer-data', t); })
      .catch(function () { renderPre('peer-data', null); });
  }

  function fetchTokio() {
    return fetchJSON('debug/tokio')
      .then(function (data) {
        renderPre('tokio-data', data);
      })
      .catch(function () { renderPre('tokio-data', null); });
  }

  var fetchers = {
    overview: fetchOverview,
    cache: fetchCache,
    pool: fetchPool,
    circuit: fetchCircuitQueue,
    metrics: fetchMetrics,
    contexts: fetchContexts,
    peer: fetchPeer,
    tokio: fetchTokio,
  };

  function fetchPanel(name) {
    var fn = fetchers[name];
    if (fn) fn();
  }

  // --- Formatting ---
  function fmtNum(n) {
    if (n === null || n === undefined) return '-';
    return Number(n).toLocaleString();
  }
  function fmtPct(r) {
    if (r === null || r === undefined) return '-';
    return (Number(r) * 100).toFixed(1) + '%';
  }
  function fmtMs(ms) {
    if (ms === null || ms === undefined) return '-';
    return Number(ms).toFixed(1) + ' ms';
  }
  function fmtDuration(secs) {
    if (secs === null || secs === undefined) return '-';
    var s = Number(secs);
    var h = Math.floor(s / 3600);
    var m = Math.floor((s % 3600) / 60);
    if (h > 0) return h + 'h ' + m + 'm';
    return m + 'm ' + (s % 60) + 's';
  }

  // --- Health check ---
  function checkHealth() {
    var dot = document.getElementById('status-dot');
    fetch(BASE + 'health', { method: 'GET' })
      .then(function (r) {
        dot.className = r.ok ? 'ok' : 'err';
        dot.title = r.ok ? 'Healthy' : 'Unhealthy';
      })
      .catch(function () {
        dot.className = 'err';
        dot.title = 'Unreachable';
      });
  }

  // --- Init ---
  checkHealth();
  fetchPanel(currentPanel);
  refreshTimer = setInterval(function () {
    checkHealth();
    fetchPanel(currentPanel);
  }, REFRESH_MS);
})();
