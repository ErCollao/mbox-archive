<script>
    import { onMount, tick } from 'svelte';

    let query = "";
    let emails = [];
    let mode = "inbox"; // "inbox" | "search"
    let searchBodies = false;
    let hasAttachments = false;
    let sel = { cat: null, status: null };
    let selOther = [];
    let selAcct = [];
    let acctFacets = [];
    let showAllLabels = false;
    let showAllStatuses = false;
    let labels = [];
    let facetMin = 0;
    let facetMax = 0;
    let sliderLo = 0;
    let sliderHi = 0;
    let afterTs = null;
    let beforeTs = null;
    let hasMore = false;
    let searchOffset = 0;
    let moreSearch = false;
    let offset = 0;
    let selectedThread = null;
    let threadMsgs = [];
    let openQuotes = {};
    let mailPane;
    let activeMsgId = null;
    let active = { loading: true, meta: null, content: "" };
    let loading = false;
    let expanded = false;
    let drawerOpen = false;
    let helpOpen = false;

    // ---------- vim-ish keyboard navigation (main view only) ----------
    const PANE_ORDER = ['left', 'list', 'email'];
    let pane = null;          // which pane owns j/k/space right now
    let listIdx = -1;         // cursor over emails
    let leftIdx = 0;          // cursor over filter buttons (index into visible DOM)
    let stickBottom = false;  // load-more in flight: ride the cursor to the new end
    let scanProg = null;        // {processed,total} while a body scan runs
    let scanPoll = null;
    let leftPane, listEl, searchInput;

    // "Full" body scans run for minutes; the backend counts them in
    // /api/status/search, so the spinner can become a counter
    async function pollScan() {
        try {
            const s = await fetch(`${API_URL}/status`).then(r => r.json());
            // total 0 = queued or identity-only scan: nothing honest to show yet
            scanProg = s.search && s.search.running && s.search.total > 0
                ? { processed: s.search.processed, total: s.search.total }
                : null;
        } catch (e) { /* transient */ }
    }
    function startScanPoll() {
        stopScanPoll();
        pollScan();
        scanPoll = setInterval(pollScan, 1200);
    }
    function stopScanPoll() {
        if (scanPoll) { clearInterval(scanPoll); scanPoll = null; }
        scanProg = null;
    }

    const TEXT_INPUT_TYPES = ['text', 'search', 'email', 'password', 'number', 'tel', 'url', ''];

    // clicking inside the sandboxed email iframe steals key focus from the
    // document — events then never reach us. Yank it back the moment focus
    // moves into a frame (focusout on the parent is the reliable signal;
    // window blur is a fallback for when the whole tab loses focus).
    function reclaimKeys(e) {
        const stolen = e?.relatedTarget?.tagName === 'IFRAME' ? e.relatedTarget : document.activeElement;
        if (stolen && stolen.tagName === 'IFRAME') setTimeout(() => stolen.blur(), 0);
    }
    onMount(() => {
        document.addEventListener('focusout', reclaimKeys);
        window.addEventListener('blur', reclaimKeys);
        const remeasure = () => updateFrame();
        window.addEventListener('resize', remeasure);
        window.addEventListener('mouseup', remeasure); // column drags end here
        return () => {
            stopScanPoll(); // never leave a timer outliving the view
            document.removeEventListener('focusout', reclaimKeys);
            window.removeEventListener('blur', reclaimKeys);
            window.removeEventListener('resize', remeasure);
            window.removeEventListener('mouseup', remeasure);
        };
    });

    function isTextEntry(el) {
        if (!el || !el.tagName) return false;
        const tag = el.tagName.toUpperCase();
        if (tag === 'TEXTAREA' || el.isContentEditable) return true;
        if (tag !== 'INPUT') return false;
        // checkboxes and sliders keep focus after clicks/drags — hjkl must
        // keep meaning navigation there, only typing fields are exempt
        return TEXT_INPUT_TYPES.includes((el.getAttribute('type') || 'text').toLowerCase());
    }

    // The email pane sits flush against the viewport edge, so neither an inset
    // shadow (covered by its own content) nor an outset one reads as a frame.
    // A fixed overlay traced onto its rect does, and never scrolls away.
    let frameStyle = "";
    async function updateFrame() {
        if (pane !== 'email') return;
        // reactive runs before the DOM flush — measuring now would freeze the
        // pre-toggle geometry (e.g. the old rect lingering over full screen)
        await tick();
        if (pane !== 'email' || !mailPane) return;
        const r = mailPane.getBoundingClientRect();
        frameStyle = `left:${r.left}px;top:${r.top}px;width:${r.width}px;height:${r.height}px;`;
    }
    $: if (pane !== null || expanded) updateFrame(); // guard lives inside

    function cyclePane(dir) {
        if (pane === null) { pane = dir > 0 ? 'list' : 'left'; }
        else {
            // stop at the edges — rotating confused people expecting a row
            // of panes, not a ring
            const i = Math.max(0, Math.min(PANE_ORDER.length - 1, PANE_ORDER.indexOf(pane) + dir));
            pane = PANE_ORDER[i];
        }
        if (pane === 'left') paintLeftCursor();
    }

    function leftItems() {
        return leftPane ? Array.from(leftPane.querySelectorAll('button.label-item')) : [];
    }

    function paintLeftCursor() {
        const items = leftItems();
        leftIdx = Math.max(0, Math.min(leftIdx, items.length - 1));
        items.forEach((el, i) => el.classList.toggle('kcursor', i === leftIdx && pane === 'left'));
        items[leftIdx]?.scrollIntoView({ block: 'nearest' });
    }

    async function moveLeft(dir) {
        const items = leftItems();
        if (!items.length) return;
        leftIdx = Math.max(0, Math.min(leftIdx + dir, items.length - 1));
        items.forEach((el, i) => el.classList.toggle('kcursor', i === leftIdx));
        items[leftIdx].scrollIntoView({ block: 'nearest' });
    }

    async function activateLeft() {
        const items = leftItems();
        const el = items[leftIdx];
        if (!el) return;
        el.click(); // same handler the mouse would fire (toggle filter or expand)
        await tick();
        paintLeftCursor();
    }

    function morePending() {
        return mode === 'inbox' ? hasMore : moreSearch;
    }

    function nextPage() {
        stickBottom = true;
        if (mode === 'inbox') loadInbox(false);
        else search(false);
    }

    async function paintListCursor() {
        await tick();
        if (!listEl) return;
        const cur = listEl.querySelector('.email-item.kcursor');
        if (!cur) return;
        const rows = listEl.querySelectorAll('.email-item');
        const moreBtn = listEl.querySelector('.load-more button');
        // at the bottom, keep the Load-more button (and thus a hint of what
        // is coming) on screen instead of gluing the last row to the edge
        const target = (moreBtn && cur === rows[rows.length - 1]) ? moreBtn : cur;
        target.scrollIntoView({ block: 'nearest' });
    }

    // the listbox is a single tab stop; Enter/Space open the cursor row and
    // stop here, arrows/j-k keep flowing to the global vim handler
    function onListKeys(e) {
        if ((e.key === 'Enter' || e.key === ' ') && emails[listIdx]) {
            e.preventDefault();
            e.stopPropagation();
            openThread(emails[listIdx].tid);
        }
    }

    async function moveList(dir) {
        if (!emails.length) return;
        if (dir === 1 && listIdx === emails.length - 1 && morePending() && !loading) {
            nextPage(); // jamming j keeps loading pages
            return;
        }
        listIdx = Math.max(0, Math.min(listIdx + dir, emails.length - 1));
        await paintListCursor();
    }

    async function jump(where) {
        if (pane === null) pane = 'list'; // cold start: gg/G own the middle, like j/k
        if (pane === 'list' && emails.length) {
            if (where === 'bottom' && listIdx === emails.length - 1 && morePending() && !loading) {
                nextPage(); // G at the end loads the next page, cursor follows
                return;
            }
            listIdx = where === 'top' ? 0 : emails.length - 1;
            await paintListCursor();
        } else if (pane === 'left') {
            leftIdx = where === 'top' ? 0 : leftItems().length - 1;
            paintLeftCursor();
        } else if (pane === 'email' && threadMsgs.length) {
            loadMessage(where === 'top' ? threadMsgs[0].id : threadMsgs[threadMsgs.length - 1].id);
        }
    }

    let gPending = 0;

    const KEYMAP = { ArrowLeft: 'h', ArrowRight: 'l', ArrowDown: 'j', ArrowUp: 'k' };

    function onKey(e) {
        if (drawerOpen) return;
        if (isTextEntry(e.target)) {
            // Escape is the escape hatch out of typing mode — from then on
            // hjkl mean navigation again.
            if (e.key === 'Escape') e.target.blur();
            return; // hjkl stay letters while typing
        }
        if (e.ctrlKey || e.metaKey || e.altKey) return;
        const k = KEYMAP[e.key] ?? e.key;
        if (k === '?') { helpOpen = !helpOpen; return; }
        if (helpOpen) {
            if (k === 'Escape') helpOpen = false;
            return; // help eats everything else while it is up
        }
        if (k === 'Escape') {
            // in the email pane, Esc first exits the reading (the Back button
            // is desktop-hidden, so this is the only way out of a thread);
            // everywhere it is a pure visual hide — cursors keep their spots
            if (pane === 'email' && selectedThread) closeThread();
            pane = null;
            return;
        }
        if (k === 'g') {
            if (Date.now() - gPending < 600) { gPending = 0; jump('top'); }
            else gPending = Date.now();
            return;
        }
        if (k === 'G') { jump('bottom'); return; }
        if (k === 'f') {
            if (pane === 'email' && selectedThread) expanded = !expanded;
            return;
        }
        gPending = 0;
        if (k === '/') {
            e.preventDefault();
            searchInput?.focus();
            searchInput?.select();
            return;
        }
        if (k === 'h' || k === 'l') { cyclePane(k === 'l' ? 1 : -1); return; }
        if (pane === null && (k === 'j' || k === 'k')) pane = 'list'; // cold start: j/k own the middle
        if (pane === null) return;
        if (k === 'j' || k === 'k') {
            e.preventDefault();
            const d = k === 'j' ? 1 : -1;
            if (pane === 'left') moveLeft(d);
            else if (pane === 'list') moveList(d);
            else if (pane === 'email' && selectedThread) stepMessage(d);
        } else if (k === 'Enter' || k === ' ') {
            e.preventDefault(); // space must never scroll the page here
            if (pane === 'left') activateLeft();
            else if (pane === 'list' && emails[listIdx]) openThread(emails[listIdx].tid);
            else if (pane === 'email' && selectedThread) expanded = !expanded;
        }
    }

    // mouse clicks own the view too: keep the keyboard cursor honest
    $: if (pane === 'list' && emails.length && listIdx >= emails.length) listIdx = emails.length - 1;
    // the left cursor is imperative DOM, so it needs its own leave-behind sweep
    $: if (pane !== 'left' && leftPane) {
        leftPane.querySelectorAll('button.label-item.kcursor')
            .forEach((el) => el.classList.remove('kcursor'));
    }

    // Use relative path since we proxy through SvelteKit server
    const API_URL = "/api";
    const DAY = 86400000;

    function totalDays() {
        return Math.max(1, Math.ceil((facetMax - facetMin) / DAY));
    }

    function isoDate(ts) {
        return new Date(ts).toISOString().slice(0, 10);
    }

    function fmtDate(iso) {
        const d = new Date(iso);
        if (isNaN(d.getTime())) return String(iso);
        return d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" })
            + " " + d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
    }

    function fmtDateOnly(ts) {
        const d = new Date(ts);
        if (isNaN(d.getTime())) return String(ts);
        return d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
    }

    function fmtMonth(ts) {
        return new Date(ts).toLocaleDateString(undefined, { year: "numeric", month: "short" });
    }

    function fmtBytes(b) {
        if (b == null) return "";
        if (b >= 1073741824) return (b / 1073741824).toFixed(1) + " GB";
        if (b >= 1048576) return (b / 1048576).toFixed(1) + " MB";
        if (b >= 1024) return Math.round(b / 1024) + " KB";
        return b + " B";
    }

    function fmtCount(n) {
        return n >= 10000 ? (n / 1000).toFixed(0) + "k" : n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n);
    }

    // ---------- filter model ----------

    const STATUS = ["Inbox", "Important", "Unread", "Opened", "Archived", "Starred", "Sent", "Drafts", "Spam", "Trash"];
    const CAT_TABS = {
        "Category Personal": "#F9AB00",
        "Category Social": "#4285F4",
        "Category Updates": "#E37400",
        "Category Forums": "#9334E6",
        "Category Reservations": "#12B5CB",
        "Category Promotions": "#0B8043"
    };
    const CAT_ORDER = Object.keys(CAT_TABS);
    const isCatTab = (n) => n in CAT_TABS;
    const dispFor = (n) => (isCatTab(n) ? n.slice("Category ".length) : n);

    function withAlpha(hex, a) {
        const r = parseInt(hex.slice(1, 3), 16);
        const g = parseInt(hex.slice(3, 5), 16);
        const b = parseInt(hex.slice(5, 7), 16);
        return `rgba(${r}, ${g}, ${b}, ${a})`;
    }

    function darken(hex, f = 0.62) {
        const c = (i) => Math.round(parseInt(hex.slice(i, i + 2), 16) * f)
            .toString(16).padStart(2, "0");
        return `#${c(1)}${c(3)}${c(5)}`;
    }

    $: catItems = labels
        .filter(([n]) => isCatTab(n))
        .sort((a, b) => CAT_ORDER.indexOf(a[0]) - CAT_ORDER.indexOf(b[0]));
    $: statusItems = labels
        .filter(([n]) => STATUS.includes(n))
        .sort((a, b) => STATUS.indexOf(a[0]) - STATUS.indexOf(b[0]));
    $: otherItems = labels.filter(([n]) => !STATUS.includes(n) && !isCatTab(n));

    // Hierarchical custom labels ("Vault/Academia/…"): top level first,
    // parents-with-children first then leaves, alphabetical within each.
    let expandedTree = {};

    function buildTree(items) {
        const roots = [];
        const index = new Map();
        for (const [full] of items) {
            let siblings = roots;
            let path = "";
            for (const seg of full.split("/")) {
                path = path ? `${path}/${seg}` : seg;
                let node = index.get(path);
                if (!node) {
                    node = { seg, full: path, children: [] };
                    index.set(path, node);
                    siblings.push(node);
                }
                siblings = node.children;
            }
            index.get(full).self = true;
        }
        return roots;
    }

    function rollup(node) {
        const pref = node.full + "/";
        let sum = 0;
        for (const [n, c] of labels) {
            if (n === node.full || n.startsWith(pref)) sum += c;
        }
        return sum;
    }

    function orderNodes(nodes) {
        const cmp = (a, b) => a.seg.localeCompare(b.seg);
        return [
            ...nodes.filter(n => n.children.length).sort(cmp),
            ...nodes.filter(n => !n.children.length).sort(cmp)
        ];
    }

    function flatten(nodes, depth, out, exp) {
        for (const n of orderNodes(nodes)) {
            out.push({
                full: n.full,
                seg: n.seg,
                depth,
                hasKids: n.children.length > 0,
                self: !!n.self,
                expanded: !!exp[n.full],
                count: rollup(n)
            });
            if (n.children.length && exp[n.full]) flatten(n.children, depth + 1, out, exp);
        }
        return out;
    }

    $: otherRoots = orderNodes(buildTree(otherItems));
    $: otherRows = (() => {
        const visible = showAllLabels ? otherRoots : otherRoots.slice(0, 5);
        // Statement-level read: Svelte does not track deps inside nested arrows.
        const exp = expandedTree;
        const out = [];
        visible.forEach(n => flatten([n], 0, out, exp));
        return out;
    })();

    function toggleExpand(full) {
        expandedTree = { ...expandedTree, [full]: !expandedTree[full] };
    }

    $: groups = [
        { head: "Categories", field: "cat", items: catItems },
        { head: "Labels", field: "other", items: otherItems },
        { head: "Status", field: "status", items: statusItems }
    ].filter(g => g.items.length);

    function pick(field, name) {
        if (field === "other") {
            selOther = selOther.includes(name)
                ? selOther.filter(n => n !== name)
                : [...selOther, name];
        } else {
            sel = { ...sel, [field]: sel[field] === name ? null : name };
        }
        drawerOpen = false;
        applyFilters();
    }

    function pickAcct(addr) {
        selAcct = selAcct.includes(addr)
            ? selAcct.filter(a => a !== addr)
            : [...selAcct, addr];
        drawerOpen = false;
        applyFilters();
    }

    // reactive derived, not a function: the compiler cannot see through
    // function calls in templates, which left the old anyFilter() call sites
    // stale (Clear button refused to appear)
    $: anyFilter = sel.cat !== null || sel.status !== null || selOther.length > 0
        || selAcct.length > 0 || afterTs !== null || beforeTs !== null || hasAttachments;

    function facetParams() {
        const p = new URLSearchParams();
        if (sel.status) p.set("status", sel.status);
        if (sel.cat) p.set("cat", sel.cat);
        if (selOther.length) p.set("other", selOther.join(","));
        if (afterTs !== null) p.set("after", isoDate(afterTs));
        if (beforeTs !== null) p.set("before", isoDate(beforeTs));
        if (hasAttachments) p.set("attachment", "1");

        if (selAcct.length) p.set("acct", selAcct.join(","));
        return p;
    }

    // direct state reads everywhere: reactive statements and templates do not
    // track dependencies through function calls (the old activeLabels()/
    // dateRangeText() calls left stale chips — the Clear-button bug in costume)
    // Division of labour: the breadcrumb names WHERE you are (category ›
    // status — also toggled off by re-clicking them in the sidebar), while
    // removable pills cover filters that are not a place (labels, addresses,
    // attachments, dates) — which is why cat/status are deliberately absent
    // from chipList: no duplicated name next to its own pill
    $: breadcrumb = [sel.cat, sel.status].filter(v => v !== null).map(dispFor).join(' › ') || 'Inbox';
    $: chipList = [
        ...selOther.map(n => ({ kind: "label", n })),
        ...(hasAttachments ? [{ kind: "att", n: "" }] : []),
        ...selAcct.map(a => ({ kind: "acct", n: a })),
        ...(afterTs !== null || beforeTs !== null ? [{ kind: "date", n: "" }] : [])
    ];
    $: dateRangeTxt = `${afterTs !== null ? fmtMonth(afterTs) : "…"} – ${beforeTs !== null ? fmtMonth(beforeTs - DAY) : "…"}`;

    function removeChip(c) {
        if (c.kind === "label") {
            selOther = selOther.filter(n => n !== c.n);
        } else if (c.kind === "att") {
            hasAttachments = false;
        } else if (c.kind === "acct") {
            selAcct = selAcct.filter(a => a !== c.n);
        } else {
            sliderLo = 0;
            sliderHi = totalDays();
            afterTs = null;
            beforeTs = null;
        }
        applyFilters();
    }

    function applyFilters() {
        refreshCounts();
        if (query.trim()) search();
        else loadInbox();
    }

    // Live facet counts: relabel the chips in place (order stays stable).
    async function refreshCounts() {
        try {
            const res = await fetch(`${API_URL}/filters?${facetParams()}`);
            if (!res.ok) return;
            const f = await res.json();
            const m = new Map(f.labels);
            labels = labels.map(([n]) => [n, m.has(n) ? m.get(n) : 0]);
        } catch (e) {
            console.error(e);
        }
    }

    function onSliderLo(e) {
        sliderLo = Math.min(Number(e.target.value), sliderHi);
        applyDates();
    }

    function onSliderHi(e) {
        sliderHi = Math.max(Number(e.target.value), sliderLo);
        applyDates();
    }

    function applyDates() {
        afterTs = sliderLo > 0 ? facetMin + sliderLo * DAY : null;
        beforeTs = sliderHi < totalDays() ? facetMin + (sliderHi + 1) * DAY : null;
        applyFilters();
    }

    function clearFilters() {
        sel = { cat: null, status: null };
        selOther = [];
        selAcct = [];
        showAllLabels = false;
        afterTs = null;
        beforeTs = null;
        hasAttachments = false;
        sliderLo = 0;
        sliderHi = totalDays();
        query = "";
        drawerOpen = false;
        loadInbox();
    }

    // ---------- data ----------

    async function loadFacets() {
        try {
            const res = await fetch(`${API_URL}/filters`);
            const f = await res.json();
            labels = f.labels;
            acctFacets = f.addresses ?? [];
            facetMin = f.min_ts;
            facetMax = f.max_ts;
            sliderLo = 0;
            sliderHi = totalDays();
        } catch (e) {
            console.error(e);
        }
    }

    async function loadInbox(reset = true) {
        if (reset) {
            offset = 0;
            emails = [];
            listIdx = -1;
            stickBottom = false; // new context (boot, filters, cleared search): no stale cursor
            if (pane === 'email') pane = 'list';
        }
        mode = "inbox";
        loading = true;
        try {
            const p = facetParams();
            p.set("offset", offset);
            p.set("limit", 100);
            const res = await fetch(`${API_URL}/messages?${p}`);
            const data = await res.json();
            emails = reset ? data : emails.concat(data);
            offset += data.length;
            hasMore = data.length === 100;
            if (stickBottom) { listIdx = emails.length - 1; stickBottom = false; paintListCursor(); }
        } catch (e) {
            console.error(e);
        } finally {
            loading = false;
        }
    }

    async function search(reset = true) {
        const trimmed = query.trim();
        if (!trimmed) {
            return loadInbox();
        }
        if (reset) {
            searchOffset = 0;
            emails = [];
            listIdx = -1;
            stickBottom = false; // a new result set is a new context: no stale cursor
            if (pane === 'email') pane = 'list';
        }
        loading = true;
        mode = "search";
        emails = [];
        selectedThread = null;
        threadMsgs = [];
        activeMsgId = null;
        if (searchBodies) startScanPoll();
        try {
            const p = facetParams();
            p.set("q", trimmed);
            if (searchBodies) p.set("body", "1");
            p.set("offset", searchOffset);
            p.set("limit", 50);
            const res = await fetch(`${API_URL}/search?${p}`);
            const data = await res.json();
            emails = reset ? data : emails.concat(data);
            searchOffset += data.length;
            moreSearch = data.length === 50;
            if (stickBottom) { listIdx = emails.length - 1; stickBottom = false; paintListCursor(); }
        } catch (e) {
            console.error(e);
        } finally {
            loading = false;
            stopScanPoll();
        }
    }

    let me = null; // { slug, name } once authenticated

    onMount(async () => {
        const res = await fetch('/api/whoami');
        if (res.status === 401) {
            window.location.href = '/welcome';
            return;
        }
        me = await res.json();
        await loadFacets();
        // Default view: the main category, when present.
        const names = labels.map(([n]) => n);
        if (names.includes("Category Personal")) sel = { ...sel, cat: "Category Personal" };
        loadInbox();
        // Deep link: /?t=<thread>&m=<message>
        const p = new URLSearchParams(window.location.search);
        const tid = p.get("t");
        if (tid) openThread(tid, p.get("m"));
    });

    // ---------- threads: one message at a time (B) ----------

    async function loadMessage(id) {
        activeMsgId = id;
        active = { loading: true, meta: null, content: "" };
        openQuotes = { ...openQuotes, [id]: false };
        setUrl(selectedThread ? selectedThread.tid : null, id);
        if (mailPane) mailPane.scrollTop = 0;
        try {
            const [content, metaRes] = await Promise.all([
                fetch(`${API_URL}/email/${id}`).then(r => r.text()),
                fetch(`${API_URL}/email/${id}/meta`).then(r => r.ok ? r.json() : null)
            ]);
            if (activeMsgId !== id) return; // user already moved on
            active = { loading: false, meta: metaRes, content };
        } catch (e) {
            if (activeMsgId === id) active = { loading: false, meta: null, content: "Error loading message" };
        }
    }

    // Deep-linking: keep the URL in sync with the opened thread/message
    // (replaceState: shareable/bookmarkable, without polluting history).
    function setUrl(tid, mid) {
        const u = new URL(window.location.href);
        if (tid) u.searchParams.set("t", tid); else u.searchParams.delete("t");
        if (mid) u.searchParams.set("m", mid); else u.searchParams.delete("m");
        history.replaceState(history.state, "", u);
    }

    async function openThread(tid, mid) {
        selectedThread = { tid, subject: "" };
        threadMsgs = [];
        activeMsgId = null;
        active = { loading: true, meta: null, content: "" };
        try {
            const res = await fetch(`${API_URL}/thread?tid=${encodeURIComponent(tid)}`);
            const d = await res.json();
            selectedThread = { tid: d.tid, subject: d.subject };
            threadMsgs = d.messages;
            const target = mid && d.messages.some(m => m.id === mid)
                ? mid
                : (d.messages.length ? d.messages[d.messages.length - 1].id : null);
            if (target) await loadMessage(target);
        } catch (e) {
            console.error(e);
        }
    }

    $: msgIndex = threadMsgs.findIndex(m => m.id === activeMsgId);

    function stepMessage(dir) {
        const next = msgIndex + dir;
        if (next >= 0 && next < threadMsgs.length) {
            loadMessage(threadMsgs[next].id);
        }
    }

    function closeThread() {
        selectedThread = null;
        if (pane === 'email') pane = 'list'; // the viewed pane vanished; hand keys back to the list
        threadMsgs = [];
        activeMsgId = null;
        expanded = false;
        setUrl(null, null);
    }

    // Quote folding for plain-text messages (HTML folds happen server-side).
    function splitQuoted(text) {
        const lines = text.split("\n");
        let first = -1;
        for (let i = 0; i < lines.length; i++) {
            const l = lines[i];
            if (/^>/.test(l)) {
                if (first === -1) first = i;
            } else if (l.trim() !== "") {
                first = -1;
            }
        }
        if (first < 0) return null;
        while (first > 0 && /^(on .+ wrote:|-+ ?original message ?-+|begin forwarded message|forwarded message)$/i.test(lines[first - 1].trim())) {
            first--;
        }
        const head = lines.slice(0, first).join("\n");
        if (!head.trim()) return null;
        return { head, quote: lines.slice(first).join("\n") };
    }
</script>

<svelte:head>
    <title>Mbox Archive</title>
</svelte:head>
<svelte:body class="app-shell" />
<svelte:window on:keydown={onKey} />

<div class="container" class:email-expanded={expanded} class:drawer-open={drawerOpen}>
    <aside class="pane pane-settings" bind:this={leftPane} class:kbd-focus={pane === 'left'}>
        <div class="title-row">
            <div class="title-left">
                {#if me}
                    <a class="who" href="/profile" title="{me.name} — profile & password">
                        <img src="/profiles/{me.slug}/portrait" alt="" on:error={(e) => e.currentTarget.remove()} />
                        <span>{me.name.charAt(0)}</span>
                    </a>
                {/if}
                <h1>Mbox Archive</h1>
            </div>
            <a class="gear" href="/maintenance" title="Maintenance & backup">⚙</a>
        </div>

        {#if facetMax > facetMin}
            <div class="dateslider">
                <div class="datelabels">
                    <span>{fmtMonth(afterTs !== null ? afterTs : facetMin)}</span>
                    <span>{fmtMonth(beforeTs !== null ? beforeTs - DAY : facetMax)}</span>
                </div>
                <div class="range-pair">
                    <div class="track"></div>
                    <div
                        class="track-fill"
                        style="left: {(sliderLo / totalDays()) * 100}%; width: {((sliderHi - sliderLo) / totalDays()) * 100}%;"
                    ></div>
                    <input type="range" min="0" max={totalDays()} value={sliderLo} on:input={onSliderLo} />
                    <input type="range" min="0" max={totalDays()} value={sliderHi} on:input={onSliderHi} />
                </div>
            </div>
        {/if}

        <div class="toggles">
            <label class="bodies">
                <input type="checkbox" bind:checked={hasAttachments} on:change={applyFilters} />
                Has attachments
            </label>
            {#if anyFilter || query}
                <button class="clear" on:click={clearFilters}>Clear</button>
            {/if}
        </div>

        {#if labels.length > 0 || acctFacets.length > 1}
            <div class="labels">
                {#each groups as g (g.field)}
                    <div class="label-group">
                        <div class="nav-head">{g.head}</div>
                        {#if g.field === 'other'}
                            {#each otherRows as r (r.full)}
                                {#if r.hasKids && r.self}
                                    <div class="tree-row" style="padding-left: {r.depth * 0.85}rem;">
                                        <button
                                            class="twisty"
                                            title={r.expanded ? 'Collapse' : 'Expand'}
                                            on:click={() => toggleExpand(r.full)}
                                        >{r.expanded ? '▾' : '▸'}</button>
                                        <button
                                            class="label-item inline"
                                            class:on={selOther.includes(r.full)}
                                            title={r.full}
                                            on:click={() => pick('other', r.full)}
                                        >
                                            <span class="lname">{r.seg}</span>
                                            <span class="count">{fmtCount(r.count)}</span>
                                        </button>
                                    </div>
                                {:else}
                                    <button
                                        class="label-item tree"
                                        class:parent={r.hasKids}
                                        class:on={!r.hasKids && selOther.includes(r.full)}
                                        style="padding-left: {0.7 + r.depth * 0.85}rem;"
                                        title={r.hasKids ? (r.expanded ? 'Collapse' : 'Expand') : r.full}
                                        on:click={() => r.hasKids ? toggleExpand(r.full) : pick('other', r.full)}
                                    >
                                        <span class="lname">{#if r.hasKids}<span class="twist">{r.expanded ? '▾' : '▸'}</span>{/if}{r.seg}</span>
                                        <span class="count">{fmtCount(r.count)}</span>
                                    </button>
                                {/if}
                            {/each}
                            {#if otherRoots.length > 5}
                                <button class="label-item show-more" on:click={() => showAllLabels = !showAllLabels}>
                                    <span class="lname">{showAllLabels ? 'Show fewer' : `Show all ${otherRoots.length} labels`}</span>
                                </button>
                            {/if}
                        {:else}
                            {@const first = g.items.slice(0, 2)}
                            {@const collapsed = g.field === 'status' && !showAllStatuses && g.items.length > 2
                                && (!sel.status || first.some(([n]) => n === sel.status))}
                            {#each collapsed ? first : g.items as [name, count] (name)}
                                {@const on = sel[g.field] === name}
                                <button
                                    class="label-item"
                                    class:on={on}
                                    style={on && isCatTab(name)
                                        ? `background: ${withAlpha(CAT_TABS[name], 0.14)}; color: ${darken(CAT_TABS[name])}; border-left-color: ${CAT_TABS[name]};`
                                        : undefined}
                                    on:click={() => pick(g.field, name)}
                                >
                                    <span class="lname">{#if isCatTab(name)}<span class="cat-dot" style="background:{CAT_TABS[name]}"></span>{/if}{dispFor(name)}</span>
                                    <span class="count">{fmtCount(count)}</span>
                                </button>
                            {/each}
                            {#if g.field === 'status' && g.items.length > 2}
                                <button class="label-item show-more" on:click={() => showAllStatuses = !showAllStatuses}>
                                    <span class="lname">{collapsed ? `Show all ${g.items.length} statuses` : 'Show fewer statuses'}</span>
                                </button>
                            {/if}
                        {/if}
                    </div>
                {/each}
                {#if acctFacets.length > 1}
                    <div class="label-group">
                        <div class="nav-head">Addresses</div>
                        {#each acctFacets as [addr, count] (addr)}
                            <button
                                class="label-item"
                                class:on={selAcct.includes(addr)}
                                title="Threads involving {addr}"
                                on:click={() => pickAcct(addr)}
                            >
                                <span class="lname">{addr}</span>
                                <span class="count">{fmtCount(count)}</span>
                            </button>
                        {/each}
                    </div>
                {/if}
            </div>
        {/if}
    </aside>

    <section class="pane pane-results" class:kbd-focus={pane === 'list'}>
        <div class="list-head">
            <div class="lh-row">
                <button class="burger" title="Settings" on:click={() => drawerOpen = true}>≡</button>
                <div class="searchbox">
                    <input
                        type="text"
                        bind:this={searchInput}
                        bind:value={query}
                        placeholder="Search subject/sender, or from:dave subject:x"
                        on:keydown={(e) => {
                            if (e.key === 'Enter') { e.target.blur(); search(); }
                        }}
                    />
                    <button on:click={search}>Search</button>
                </div>
                <label class="full-cb" title="Include message bodies — scans every message, takes minutes">
                    <input type="checkbox" bind:checked={searchBodies} on:change={applyFilters} />
                    Full
                </label>
            </div>
            <div class="lh-row lh-sub">
                <span class="status">
                    {#if loading && emails.length === 0}
                        Searching…{#if scanProg}<span class="scanprog">{fmtCount(scanProg.processed)} / {fmtCount(scanProg.total)} bodies scanned</span>{/if}
                    {:else if mode === 'search'}
                        {emails.length} conversation{emails.length === 1 ? '' : 's'}
                    {:else}
                        {breadcrumb}
                    {/if}
                    {#each chipList as c (c.kind + c.n)}
                        <button class="af-chip" title="Remove filter" on:click={() => removeChip(c)}>
                            {c.kind === 'label' ? dispFor(c.n) : c.kind === 'att' ? 'Has attachments' : c.kind === 'acct' ? c.n : dateRangeTxt} ×
                        </button>
                    {/each}
                </span>
                <span class="order">newest first</span>
            </div>
        </div>

        <div
            class="email-list"
            bind:this={listEl}
            role="listbox"
            aria-label="Email conversations"
            tabindex="0"
            aria-activedescendant={pane === 'list' && listIdx >= 0 ? 'email-row-' + listIdx : null}
            on:focus={() => { if (pane !== 'list') pane = 'list'; }}
            on:keydown={onListKeys}
        >
            {#if loading && emails.length === 0}
                <div class="loading">
                    {#if mode === 'search' && searchBodies}
                        Scanning every message body — minutes on a large archive…
                        {#if scanProg}<span class="scanprog">{fmtCount(scanProg.processed)} / {fmtCount(scanProg.total)}</span>{/if}
                    {:else}
                        Loading...
                    {/if}
                </div>
            {:else if emails.length === 0}
                {#if mode === 'search'}
                    <div class="empty">
                        No results.
                        <button on:click={() => clearFilters()}>Back to inbox</button>
                    </div>
                {:else}
                    <div class="empty">No emails match. Adjust filters, or drop .mbox files in data/ and re-scan.</div>
                {/if}
            {:else}
                {#each emails as t, i (t.tid)}
                    <!-- svelte-ignore a11y-click-events-have-key-events a11y-interactive-supports-focus -->
                    <!-- the listbox container owns the keyboard; options are
                         announced via aria-activedescendant, not tab stops -->
                    <div
                        id="email-row-{i}"
                        class="email-item"
                        role="option"
                        aria-selected={selectedThread && selectedThread.tid === t.tid}
                        class:selected={selectedThread && selectedThread.tid === t.tid}
                        class:kcursor={pane === 'list' && i === listIdx}
                        on:click={() => { listIdx = i; openThread(t.tid); }}
                    >
                        <div class="subject" title={t.subject}>{#if t.attachments}<span class="clip" title="Has attachments">📎</span>{/if}{t.subject || '(No Subject)'}{#if t.count > 1}<span class="badge">{mode === 'search' && t.matched ? `${t.matched}/${t.count}` : t.count}</span>{/if}</div>
                        <div class="meta">
                            <span class="from" title={t.from}>{t.from}</span>
                            <span class="date">{fmtDate(t.date)}</span>
                            {#each t.accts ?? [] as a (a)}
                                <span class="acct-tag" title="Involves this address">{a}</span>
                            {/each}
                        </div>
                    </div>
                {/each}
                {#if mode === 'inbox' && hasMore}
                    <div class="load-more">
                        <button on:click={() => loadInbox(false)} disabled={loading}>{loading ? 'Loading…' : 'Load 100 more'}</button>
                    </div>
                {:else if mode === 'search' && moreSearch}
                    <div class="load-more">
                        <button on:click={() => search(false)} disabled={loading}>{loading ? 'Loading…' : 'Load 50 more matches'}</button>
                        {#if searchBodies}
                            <div class="recheck">(re-scans the archive for the next page — takes minutes)</div>
                        {/if}
                    </div>
                {/if}
            {/if}
        </div>
    </section>

    <section class="pane pane-email" bind:this={mailPane} class:has-email={selectedThread !== null} class:kbd-focus={pane === 'email'}>
        {#if selectedThread}
            <div class="email-view">
                <div class="msg-head">
                    <button class="head-btn back-btn" on:click={closeThread}>Back</button>
                    <div class="head-titles">
                        <h2>{selectedThread.subject || '(No Subject)'}</h2>
                        {#if threadMsgs.length > 1}
                            <span class="thread-count">{threadMsgs.length} messages</span>
                        {/if}
                    </div>
                    <button class="head-btn fs-toggle" on:click={() => expanded = !expanded}>
                        {expanded ? 'Restore' : 'Full screen'}
                    </button>
                </div>

                {#if threadMsgs.length > 1}
                    <div class="msg-strip">
                        {#each threadMsgs as m (m.id)}
                            <button
                                class="msg-chip"
                                class:active={activeMsgId === m.id}
                                title={m.from + ' — ' + fmtDateOnly(m.date)}
                                on:click={() => loadMessage(m.id)}
                            >{m.from.split(' <')[0]} · {fmtDateOnly(m.date)}{#if m.attachments} ·📎{/if}</button>
                        {/each}
                        <span class="strip-nav">
                            <button
                                class="nav-btn"
                                title="Older message"
                                disabled={msgIndex <= 0}
                                on:click={() => stepMessage(-1)}
                            >↑</button>
                            <span class="page-count">{Math.max(1, msgIndex + 1)} / {threadMsgs.length}</span>
                            <button
                                class="nav-btn"
                                title="Newer message"
                                disabled={msgIndex >= threadMsgs.length - 1}
                                on:click={() => stepMessage(1)}
                            >↓</button>
                        </span>
                    </div>
                {/if}

                {#if active.loading}
                    <div class="loading">Loading message…</div>
                {:else}
                    {@const md = active}
                    {#if md.meta}
                        <div class="active-info">
                            <span class="ai-from">{md.meta.from}</span>
                            <span class="ai-date">{fmtDate(md.meta.date)}</span>
                        </div>
                        {#if md.meta.to.length}
                            <div class="active-sub" title={md.meta.to.join(', ')}>
                                to {md.meta.to.map(t => t.split(' <')[0]).slice(0, 4).join(', ')}{md.meta.to.length > 4 ? ` +${md.meta.to.length - 4}` : ''}
                            </div>
                        {/if}
                        {#if md.meta.labels.length}
                            <div class="label-row header-labels">
                                {#each md.meta.labels as l (l)}
                                    <span class="chip sm">{dispFor(l)}</span>
                                {/each}
                            </div>
                        {/if}
                        {#if md.meta.attachments.some(a => !a.is_inline)}
                            <div class="attachments">
                                <h3>
                                    Attachments
                                    {#if md.meta.inline_images}
                                        <span class="note">({md.meta.inline_images} image{md.meta.inline_images === 1 ? '' : 's'} also shown inline)</span>
                                    {/if}
                                </h3>
                                {#each md.meta.attachments.filter(a => !a.is_inline) as a (a.part_index)}
                                    <a class="download" href="{API_URL}/attachment/{activeMsgId}/{a.part_index}">
                                        Download: {a.filename} ({fmtBytes(a.size)}, {a.content_type})
                                    </a>
                                {/each}
                            </div>
                        {/if}
                    {/if}

                    {#if md.content.trim().startsWith('<')}
                        <iframe
                            class="msg-frame"
                            srcdoc={md.content}
                            title="Email content"
                            sandbox="allow-popups"
                        ></iframe>
                    {:else if splitQuoted(md.content)}
                        {@const q = splitQuoted(md.content)}
                        <pre>{q.head}</pre>
                        <button
                            class="q-toggle"
                            on:click={() => openQuotes = { ...openQuotes, [activeMsgId]: !openQuotes[activeMsgId] }}
                        >{openQuotes[activeMsgId] ? 'Hide quoted text' : `Show quoted text (${q.quote.split('\n').length} lines)`}</button>
                        {#if openQuotes[activeMsgId]}
                            <pre class="quote">{q.quote}</pre>
                        {/if}
                    {:else}
                        <pre>{md.content}</pre>
                    {/if}
                {/if}
            </div>
        {:else}
            <div class="placeholder">
                Select a conversation to view
            </div>
        {/if}
    </section>
</div>

{#if drawerOpen}
    <button class="drawer-backdrop" aria-label="Close settings" on:click={() => drawerOpen = false}></button>
{/if}

<div class="kbd-frame" class:show={pane === 'email'} style={frameStyle}></div>

<button class="help-key" title="Keyboard shortcuts (?)" aria-label="Toggle keyboard shortcuts" on:click={() => helpOpen = !helpOpen}>?</button>

{#if helpOpen}
    <div class="help-backdrop" role="presentation" on:click|self={() => helpOpen = false}>
        <div class="help-card" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts">
            <h3>Keyboard shortcuts</h3>
            <p class="sub">Active in the archive view only — and never while your cursor is typing in a field (there, <span class="mono">h j k l</span> are just letters).</p>
            <div class="shortcut-rows">
                <div><span class="keys"><kbd>h</kbd><kbd>←</kbd> / <kbd>l</kbd><kbd>→</kbd></span><span>focus previous / next pane</span></div>
                <div><span class="keys"><kbd>j</kbd><kbd>↓</kbd> / <kbd>k</kbd><kbd>↑</kbd></span><span>move within the focused pane</span></div>
                <div><span class="keys"><kbd>Enter</kbd> / <kbd>Space</kbd></span><span>open email · apply highlighted filter · toggle full screen (email pane)</span></div>
                <div><span class="keys"><kbd>g</kbd><kbd>g</kbd> / <kbd>G</kbd></span><span>jump to first / last item of the focused pane</span></div>
                <div><span class="keys"><kbd>/</kbd></span><span>search — <kbd>Enter</kbd> runs it, <kbd>Esc</kbd> leaves the field</span></div>
                <div><span class="keys"><kbd>f</kbd></span><span>full screen the open email</span></div>
                <div><span class="keys"><kbd>Esc</kbd></span><span>leave an open email · otherwise hide all highlights</span></div>
                <div><span class="keys"><kbd>?</kbd></span><span>this help</span></div>
            </div>
            <button on:click={() => helpOpen = false}>Close</button>
        </div>
    </div>
{/if}

<style>
    :global(body) {
        margin: 0;
        font-family: system-ui, -apple-system, sans-serif;
    }
    :global(body.app-shell) {
        height: 100vh;
        overflow: hidden;
    }
    .container {
        display: grid;
        grid-template-columns: 270px minmax(360px, 440px) 1fr;
        height: 100vh;
    }
    .container.email-expanded {
        grid-template-columns: 1fr;
    }
    .email-expanded .pane-settings,
    .email-expanded .pane-results {
        display: none;
    }
    .pane {
        height: 100vh;
        overflow-y: auto;
        /* reserved so kbd focus can paint a solid top strip without ever
           shifting content or losing to sticky children */
        border-top: 4px solid transparent;
    }
    .pane-settings {
        background: #f2f2f2;
        border-right: 1px solid #ddd;
        display: flex;
        flex-direction: column;
        position: relative;
    }
    .title-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding: 1.5rem 1rem 0.5rem;
    }
    .title-row h1 {
        margin: 0;
        font-size: 1.05rem;
    }
    .title-left {
        display: flex;
        align-items: center;
        gap: 0.55rem;
    }
    .who {
        position: relative;
        width: 27px;
        height: 27px;
        border-radius: 50%;
        border: none;
        padding: 0;
        cursor: pointer;
        background: #1a73e8;
        color: white;
        font-size: 0.8rem;
        font-weight: 600;
        display: flex;
        align-items: center;
        justify-content: center;
        overflow: hidden;
        flex-shrink: 0;
    }
    .who:hover { box-shadow: 0 0 0 2px rgba(26, 115, 232, 0.3); }
    .who { text-decoration: none; }
    .who img {
        position: absolute;
        inset: 0;
        width: 100%;
        height: 100%;
        object-fit: cover;
    }
    .gear {
        text-decoration: none;
        display: inline-block;
        border: none;
        background: transparent;
        font-size: 1.15rem;
        line-height: 1;
        cursor: pointer;
        padding: 0.3rem 0.45rem;
        margin-right: -0.45rem;
        border-radius: 4px;
        color: #444;
    }
    .gear:hover {
        background: #e2e2e2;
    }
    .pane-results {
        background: #f7f7f7;
        border-right: 1px solid #ddd;
        display: flex;
        flex-direction: column;
        height: 100vh;
    }
    .list-head {
        display: flex;
        flex-direction: column;
        gap: 0.45rem;
        padding: 1rem 1rem 0.6rem;
        font-size: 0.8rem;
        color: #444;
        border-bottom: 1px solid #ddd;
        background: #f2f2f2;
        position: sticky;
        top: 0;
        z-index: 2;
    }
    .lh-row {
        display: flex;
        align-items: center;
        gap: 0.5rem;
    }
    .searchbox {
        display: flex;
        align-items: stretch;
        flex: 1;
        min-width: 0;
        border: 1px solid #d0d3d8;
        border-radius: 8px;
        background: #fff;
        overflow: hidden;
    }
    .searchbox:focus-within { border-color: #1a73e8; box-shadow: 0 0 0 3px rgba(26, 115, 232, 0.16); }
    .searchbox input[type="text"] {
        flex: 1;
        min-width: 0;
        border: none;
        outline: none;
        background: transparent;
        padding: 0.65rem 0.8rem;
        font-size: 0.95rem;
        font-family: inherit;
        color: #202124;
    }
    .searchbox button {
        border: none;
        border-left: 1px solid #e8eaed;
        background: transparent;
        color: #80868b;
        cursor: pointer;
        padding: 0 0.7rem;
        font-size: 0.95rem;
        font-family: inherit;
        white-space: nowrap;
    }
    .searchbox button:hover { background: #f1f3f4; color: #202124; }
    .lh-sub {
        justify-content: space-between;
        font-weight: 600;
    }
    .status {
        display: flex;
        align-items: center;
        gap: 0.4rem;
        flex-wrap: wrap;
    }
    .scanprog {
        color: #1a73e8;
        font-weight: 600;
    }
    .af-chip {
        background: #1a73e8;
        color: white;
        border: none;
        border-radius: 10px;
        padding: 0.1rem 0.5rem;
        font-size: 0.7rem;
        font-weight: normal;
        cursor: pointer;
    }
    .full-cb {
        font-size: 0.75rem;
        color: #555;
        display: flex;
        align-items: center;
        gap: 0.25rem;
        cursor: pointer;
        white-space: nowrap;
    }
    .list-head .order {
        font-weight: normal;
        color: #999;
        font-size: 0.72rem;
    }
    .pane-email {
        background: white;
    }
    .dateslider {
        padding: 0.4rem 1rem;
    }
    .datelabels {
        display: flex;
        justify-content: space-between;
        font-size: 0.75rem;
        color: #555;
    }
    .range-pair {
        position: relative;
        height: 24px;
    }
    .track {
        position: absolute;
        left: 0;
        right: 0;
        top: 10px;
        height: 4px;
        border-radius: 2px;
        background: #ccc;
    }
    .track-fill {
        position: absolute;
        top: 10px;
        height: 4px;
        border-radius: 2px;
        background: #333;
    }
    .range-pair input[type="range"] {
        position: absolute;
        left: 0;
        right: 0;
        top: 0;
        width: 100%;
        pointer-events: none;
        appearance: none;
        background: transparent;
        height: 24px;
        z-index: 1;
        margin: 0;
    }
    .range-pair input[type="range"]::-webkit-slider-thumb {
        appearance: none;
        pointer-events: auto;
        width: 14px;
        height: 14px;
        border-radius: 50%;
        background: #333;
        border: 2px solid white;
        box-shadow: 0 1px 3px rgba(0, 0, 0, 0.4);
        cursor: ew-resize;
    }
    .range-pair input[type="range"]::-moz-range-thumb {
        pointer-events: auto;
        width: 12px;
        height: 12px;
        border-radius: 50%;
        background: #333;
        border: 2px solid white;
        cursor: ew-resize;
    }
    .toggles {
        padding: 0.2rem 1rem 0.5rem;
        display: flex;
        align-items: center;
        justify-content: space-between;
    }
    .bodies {
        font-size: 0.75rem;
        color: #555;
        display: flex;
        align-items: center;
        gap: 0.3rem;
        flex-basis: 100%;
    }
    .clear {
        font-size: 0.75rem;
        padding: 0.2rem 0.5rem;
        cursor: pointer;
    }
    .labels {
        padding: 0 0 0.2rem;
    }
    .label-group {
        margin: 0 1rem 0.45rem;
        border: 1px solid #e2e2e2;
        border-radius: 4px;
        background: white;
        overflow: hidden;
    }
    .nav-head {
        font-size: 0.66rem;
        text-transform: uppercase;
        letter-spacing: 0.05em;
        color: #888;
        padding: 0.35rem 0.7rem 0.2rem;
        background: #fafafa;
        border-bottom: 1px solid #f0f0f0;
    }
    .label-item {
        display: flex;
        justify-content: space-between;
        align-items: center;
        width: 100%;
        padding: 0.3rem 0.7rem;
        font-size: 0.8rem;
        border: none;
        border-bottom: 1px solid #f0f0f0;
        background: transparent;
        cursor: pointer;
        text-align: left;
    }
    .label-item:hover {
        background: #f0f0f0;
    }
    .label-item.on {
        background: #d3e3fd;
        color: #1c3aa9;
        font-weight: 600;
        border-left: 3px solid #1a73e8;
        padding-left: 0.45rem;
    }
    .label-item.on .lname::before {
        content: "✓ ";
        color: currentColor;
    }
    .label-item .count {
        opacity: 0.55;
        font-size: 0.72rem;
        margin-left: 0.4rem;
    }
    .cat-dot {
        display: inline-block;
        width: 8px;
        height: 8px;
        border-radius: 50%;
        margin-right: 0.45rem;
        vertical-align: middle;
    }
    .label-item.parent .lname {
        font-weight: 600;
        color: #333;
    }
    .twist {
        display: inline-block;
        width: 1rem;
        color: #888;
        font-size: 0.7rem;
    }
    .tree-row {
        display: flex;
        align-items: stretch;
        border-bottom: 1px solid #f0f0f0;
    }
    .twisty {
        border: none;
        background: transparent;
        color: #888;
        font-size: 0.7rem;
        padding: 0 0.3rem 0 0.55rem;
        cursor: pointer;
        align-self: center;
    }
    .twisty:hover {
        color: #333;
    }
    .label-item.inline {
        flex: 1;
        border-bottom: none;
        min-width: 0;
    }
    .show-more .lname {
        color: #06c;
        font-size: 0.75rem;
    }
    .email-list {
        flex: 1;
        overflow-y: auto;
    }
    .email-item {
        padding: 0.8rem 1rem;
        border-bottom: 1px solid #eee;
        cursor: pointer;
        overflow: hidden;
        background: white;
    }
    .email-item:hover {
        background: #eee;
    }
    /* keyboard navigation: focused pane lifts with a strip and drop shadow.
       No inset ring: panes' opaque content paints over it unevenly — the
       email pane gets a real frame via the fixed .kbd-frame overlay instead */
    .pane.kbd-focus {
        position: relative;
        z-index: 8;
        border-top-color: #1a73e8;
        box-shadow: 0 2px 18px rgba(0, 0, 0, 0.18);
    }
    .kbd-frame {
        position: fixed;
        pointer-events: none;
        z-index: 99;
        box-shadow: inset 0 0 0 3px rgba(26, 115, 232, 0.75);
        visibility: hidden;
    }
    .kbd-frame.show {
        visibility: visible;
    }
    .help-key {
        position: fixed;
        left: 0.8rem;
        bottom: 0.8rem;
        z-index: 15;
        width: 1.7rem;
        height: 1.7rem;
        border-radius: 50%;
        border: 1px solid #d0d3d8;
        background: #fff;
        color: #80868b;
        font-size: 0.85rem;
        cursor: pointer;
        box-shadow: 0 1px 4px rgba(0, 0, 0, 0.12);
    }
    .help-key:hover { color: #1a73e8; border-color: #1a73e8; }
    .help-backdrop {
        position: fixed;
        inset: 0;
        z-index: 100;
        background: rgba(32, 33, 36, 0.45);
        display: flex;
        align-items: center;
        justify-content: center;
    }
    .help-card {
        background: #fff;
        border-radius: 10px;
        padding: 1.4rem 1.6rem;
        width: min(560px, 92vw);
        max-height: 86vh;
        overflow-y: auto;
        box-shadow: 0 8px 40px rgba(0, 0, 0, 0.3);
        color: #333;
    }
    .help-card h3 { margin: 0 0 0.4rem; font-size: 1.05rem; }
    .help-card .sub { font-size: 0.78rem; color: #666; margin: 0 0 1rem; }
    .shortcut-rows { display: flex; flex-direction: column; gap: 0.55rem; font-size: 0.82rem; }
    .shortcut-rows > div { display: flex; gap: 0.9rem; align-items: baseline; }
    .shortcut-rows .keys { flex: 0 0 11.5rem; white-space: nowrap; }
    kbd, .mono {
        display: inline-block;
        background: #f1f3f4;
        border: 1px solid #dadce0;
        border-bottom-width: 2px;
        border-radius: 4px;
        padding: 0 0.35rem;
        font-family: ui-monospace, monospace;
        font-size: 0.75rem;
        color: #202124;
    }
    .help-card > button {
        margin-top: 1.2rem;
        border: 1px solid #d0d3d8;
        background: #fff;
        border-radius: 6px;
        padding: 0.35rem 0.9rem;
        cursor: pointer;
        font-size: 0.8rem;
    }
    .help-card > button:hover { border-color: #1a73e8; color: #1a73e8; }
    .email-item.kcursor {
        outline: 2px solid #1a73e8;
        outline-offset: -2px;
        background: #e8f0fe;
    }
    /* :global so the imperatively-toggled class survives Svelte's pruning;
       the element name lifts specificity over the scoped base rule */
    :global(button.label-item.kcursor) {
        outline: 2px solid #1a73e8;
        outline-offset: -2px;
        /* !important: Svelte's scoped base rules carry a double class token
           and would otherwise out-specify this transient cursor */
        background: #e8f0fe !important;
    }
    .email-item.selected {
        background: #e0e0e0;
        border-left: 4px solid #333;
    }
    .subject {
        font-weight: 500;
        margin-bottom: 0.2rem;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }
    .clip {
        font-size: 0.8rem;
        margin-right: 0.35rem;
        opacity: 0.75;
    }
    .acct-tag {
        flex-shrink: 0;
        padding: 0 0.4rem;
        border-radius: 8px;
        border: 1px solid #d3e3fd;
        color: #1c3aa9;
        background: #fff;
        font-size: 0.66rem;
        white-space: nowrap;
    }
    .badge {
        display: inline-block;
        margin-left: 0.45rem;
        padding: 0 0.4rem;
        border-radius: 8px;
        background: #d3e3fd;
        color: #1c3aa9;
        font-size: 0.7rem;
        font-weight: 600;
        vertical-align: middle;
    }
    .meta {
        font-size: 0.8rem;
        color: #666;
        display: flex;
        justify-content: space-between;
        gap: 0.5rem;
    }
    .from {
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        flex: 1;
    }
    .date {
        white-space: nowrap;
    }
    .load-more {
        padding: 1rem;
        text-align: center;
    }
    .load-more button {
        padding: 0.5rem 1rem;
        cursor: pointer;
    }
    .load-more .recheck {
        font-size: 0.72rem;
        color: #888;
        margin-top: 0.3rem;
    }
    .email-view {
        padding: 0 2rem 1.5rem;
    }
    .msg-head {
        position: sticky;
        top: 0;
        z-index: 6;
        background: #fff;
        display: flex;
        align-items: center;
        gap: 0.7rem;
        padding: 1.5rem 2rem 0.5rem;
        margin: 0 -2rem 0.6rem;
    }
    .head-btn {
        flex-shrink: 0;
        font-size: 0.72rem;
        padding: 0.22rem 0.65rem;
        cursor: pointer;
        border: 1px solid #bbb;
        border-radius: 4px;
        background: white;
    }
    .head-titles {
        flex: 1;
        min-width: 0;
        display: flex;
        align-items: baseline;
        gap: 0.7rem;
    }
    .msg-head h2 {
        margin: 0;
        font-size: 1.02rem;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        min-width: 0;
    }
    .thread-count {
        color: #888;
        font-size: 0.72rem;
        white-space: nowrap;
        flex-shrink: 0;
    }
    .msg-strip {
        display: flex;
        flex-wrap: wrap;
        gap: 0.3rem;
        margin-bottom: 0.7rem;
    }
    .msg-chip {
        border: 1px solid #ddd;
        background: #fafafa;
        border-radius: 14px;
        font-size: 0.74rem;
        padding: 0.18rem 0.6rem;
        cursor: pointer;
        color: #555;
        white-space: nowrap;
    }
    .msg-chip:hover {
        background: #eee;
    }
    .msg-chip.active {
        background: #d3e3fd;
        border-color: #a9c8f5;
        color: #1c3aa9;
        font-weight: 600;
    }
    .strip-nav {
        margin-left: auto;
        display: flex;
        gap: 0.25rem;
    }
    .nav-btn {
        width: 1.9rem;
        height: 1.75rem;
        border: 1px solid #ddd;
        border-radius: 4px;
        background: white;
        cursor: pointer;
        font-size: 0.95rem;
        line-height: 1;
        color: #555;
    }
    .nav-btn:hover:not(:disabled) {
        background: #eee;
    }
    .nav-btn:disabled {
        opacity: 0.35;
        cursor: default;
    }
    .page-count {
        font-size: 0.75rem;
        color: #666;
        min-width: 3rem;
        text-align: center;
        line-height: 1.75rem;
        font-variant-numeric: tabular-nums;
    }
    .active-info {
        display: flex;
        align-items: baseline;
        gap: 0.7rem;
        font-size: 0.85rem;
        margin-bottom: 0.1rem;
    }
    .ai-from {
        font-weight: 600;
    }
    .ai-date {
        margin-left: auto;
        color: #777;
        font-size: 0.75rem;
        white-space: nowrap;
    }
    .active-sub {
        font-size: 0.78rem;
        color: #666;
        margin-bottom: 0.3rem;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }
    .label-row {
        display: flex;
        flex-wrap: wrap;
        gap: 0.3rem;
    }
    .header-labels {
        margin: 0.2rem 0 0.5rem;
    }
    .chip.sm {
        cursor: default;
        background: #eef;
    }
    .chip {
        font-size: 0.72rem;
        padding: 0.15rem 0.5rem;
        border: 1px solid #bbb;
        border-radius: 10px;
        background: white;
    }
    .attachments {
        border: 1px solid #ddd;
        border-radius: 4px;
        background: #fafafa;
        padding: 0.5rem 1rem;
        margin-bottom: 0.8rem;
    }
    .attachments h3 {
        margin: 0.3rem 0;
        font-size: 0.9rem;
    }
    .attachments .note {
        font-weight: normal;
        font-size: 0.75rem;
        color: #777;
    }
    .download {
        display: inline-block;
        margin: 0.15rem 0.5rem 0.15rem 0;
        font-size: 0.85rem;
        color: #06c;
    }
    .msg-frame {
        width: 100%;
        height: 65vh;
        border: 1px solid #eee;
        border-radius: 6px;
        background: white;
    }
    .email-expanded .msg-frame {
        height: calc(100vh - 220px);
    }
    .email-view pre {
        white-space: pre-wrap;
        overflow-wrap: break-word;
        font-family: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
        font-size: 0.92rem;
        line-height: 1.55;
        background: transparent;
        padding: 0.4rem 0.2rem;
        margin: 0;
    }
    .q-toggle {
        border: none;
        background: transparent;
        color: #06c;
        font-size: 0.8rem;
        cursor: pointer;
        padding: 0.2rem 0;
        text-align: left;
    }
    pre.quote {
        opacity: 0.8;
        border-left: 3px solid #ddd;
    }
    .placeholder {
        color: #999;
        text-align: center;
        margin-top: 3rem;
    }
    .loading, .empty {
        padding: 2rem;
        text-align: center;
        color: #666;
    }
    .empty button {
        margin-left: 0.5rem;
        cursor: pointer;
    }
    @media (min-width: 901px) {
        .back-btn {
            display: none;
        }
    }
    @media (max-width: 900px) {
        .fs-toggle {
            display: none;
        }
    }
    @media (max-width: 640px) {
        .container {
            grid-template-columns: 1fr;
            grid-template-rows: auto 1fr;
        }
        .container.email-expanded {
            grid-template-columns: 1fr;
        }
        .email-expanded .pane-settings,
        .email-expanded .pane-results {
            display: flex;
        }
        .pane {
            height: auto;
            overflow: visible;
        }
        .pane-results {
            display: contents;
        }
        /* .lh-row prefix beats the base `.burger{display:none}` that lives
           lower in the stylesheet (equal-specificity would lose to it) */
        .lh-row .burger {
            display: inline-block;
        }
        .email-list {
            height: 100%;
            min-height: 0;
            overflow-y: auto;
        }
        .pane-settings {
            position: fixed;
            top: 0;
            bottom: 0;
            left: 0;
            width: 280px;
            height: 100vh;
            overflow-y: auto;
            z-index: 60;
            transform: translateX(-105%);
            transition: transform 0.18s ease;
            box-shadow: 2px 0 12px rgba(0, 0, 0, 0.25);
        }
        .container.drawer-open .pane-settings {
            transform: none;
        }
        /* element-name prefix beats the base `.drawer-backdrop{display:none}`
           that sits below this media block (see burger note above) */
        button.drawer-backdrop {
            display: block;
        }
        .pane-email {
            position: fixed;
            inset: 0;
            height: 100vh;
            overflow-y: auto;
            z-index: 50;
            background: white;
        }
        .pane-email:not(.has-email) {
            display: none;
        }
        .pane-email .email-view {
            padding: 0 1.4rem 1rem;
        }
        .msg-head {
            margin-left: -1.4rem;
            margin-right: -1.4rem;
            padding-left: 1.4rem;
            padding-right: 1.4rem;
        }
        .msg-frame {
            height: calc(100vh - 260px);
            min-height: 300px;
        }
    }
    .burger {
        display: none;
        border: none;
        background: transparent;
        font-size: 1.05rem;
        cursor: pointer;
        padding: 0.1rem 0.35rem;
        border-radius: 4px;
        color: #333;
    }
    .burger:hover {
        background: #eee;
    }
    .drawer-backdrop {
        display: none;
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.35);
        border: none;
        z-index: 55;
    }
</style>
