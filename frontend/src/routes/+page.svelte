<script>
    import { onMount } from 'svelte';

    let query = "";
    let emails = [];
    let mode = "inbox"; // "inbox" | "search"
    let searchBodies = false;
    let hasAttachments = false;
    let sel = { cat: null, status: null };
    let selOther = [];
    let showAllLabels = false;
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

    function activeLabels() {
        return [sel.cat, sel.status, ...selOther].filter(v => v !== null);
    }

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

    function fieldFor(name) {
        for (const [k, v] of Object.entries(sel)) if (v === name) return k;
        return null;
    }

    function anyFilter() {
        return activeLabels().length > 0 || afterTs !== null || beforeTs !== null || hasAttachments;
    }

    function facetParams() {
        const p = new URLSearchParams();
        if (sel.status) p.set("status", sel.status);
        if (sel.cat) p.set("cat", sel.cat);
        if (selOther.length) p.set("other", selOther.join(","));
        if (afterTs !== null) p.set("after", isoDate(afterTs));
        if (beforeTs !== null) p.set("before", isoDate(beforeTs));
        if (hasAttachments) p.set("attachment", "1");
        return p;
    }

    $: chipList = [
        ...activeLabels().map(n => ({ kind: "label", n })),
        ...(hasAttachments ? [{ kind: "att", n: "" }] : []),
        ...(afterTs !== null || beforeTs !== null ? [{ kind: "date", n: "" }] : [])
    ];

    function dateRangeText() {
        const a = afterTs !== null ? fmtMonth(afterTs) : "…";
        const b = beforeTs !== null ? fmtMonth(beforeTs - DAY) : "…";
        return `${a} – ${b}`;
    }

    function removeChip(c) {
        if (c.kind === "label") {
            if (selOther.includes(c.n)) {
                selOther = selOther.filter(n => n !== c.n);
            } else {
                const f = fieldFor(c.n);
                if (f) sel = { ...sel, [f]: null };
            }
        } else if (c.kind === "att") {
            hasAttachments = false;
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
        }
        loading = true;
        mode = "search";
        emails = [];
        selectedThread = null;
        threadMsgs = [];
        activeMsgId = null;
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
        } catch (e) {
            console.error(e);
        } finally {
            loading = false;
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

<div class="container" class:email-expanded={expanded} class:drawer-open={drawerOpen}>
    <aside class="pane pane-settings">
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
            {#if anyFilter() || query}
                <button class="clear" on:click={clearFilters}>Clear</button>
            {/if}
        </div>

        {#if labels.length > 0}
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
                            {#each g.items as [name, count] (name)}
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
                        {/if}
                    </div>
                {/each}
            </div>
        {/if}
    </aside>

    <section class="pane pane-results">
        <div class="list-head">
            <div class="lh-row">
                <button class="burger" title="Settings" on:click={() => drawerOpen = true}>≡</button>
                <input
                    type="text"
                    bind:value={query}
                    placeholder="Search subject/sender, or from:dave subject:x"
                    on:keydown={(e) => e.key === 'Enter' && search()}
                />
                <button on:click={search}>Search</button>
                <label class="full-cb" title="Include message bodies — scans every message, takes minutes">
                    <input type="checkbox" bind:checked={searchBodies} on:change={applyFilters} />
                    Full
                </label>
            </div>
            <div class="lh-row lh-sub">
                <span class="status">
                    {#if loading && emails.length === 0}
                        Searching...
                    {:else if mode === 'search'}
                        {emails.length} conversation{emails.length === 1 ? '' : 's'}
                    {:else}
                        {activeLabels().map(dispFor).join(' › ') || 'Inbox'}
                    {/if}
                    {#each chipList as c (c.kind + c.n)}
                        <button class="af-chip" title="Remove filter" on:click={() => removeChip(c)}>
                            {c.kind === 'label' ? dispFor(c.n) : c.kind === 'att' ? 'Has attachments' : dateRangeText()} ×
                        </button>
                    {/each}
                </span>
                <span class="order">newest first</span>
            </div>
        </div>

        <div class="email-list">
            {#if loading && emails.length === 0}
                <div class="loading">
                    {mode === 'search' && searchBodies
                        ? 'Scanning every message body — minutes on a large archive...'
                        : 'Loading...'}
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
                {#each emails as t (t.tid)}
                    <div
                        class="email-item"
                        class:selected={selectedThread && selectedThread.tid === t.tid}
                        on:click={() => openThread(t.tid)}
                    >
                        <div class="subject" title={t.subject}>{#if t.attachments}<span class="clip" title="Has attachments">📎</span>{/if}{t.subject || '(No Subject)'}{#if t.count > 1}<span class="badge">{mode === 'search' && t.matched ? `${t.matched}/${t.count}` : t.count}</span>{/if}</div>
                        <div class="meta">
                            <span class="from" title={t.from}>{t.from}</span>
                            <span class="date">{fmtDate(t.date)}</span>
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

    <section class="pane pane-email" bind:this={mailPane} class:has-email={selectedThread !== null}>
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
        padding: 1rem 1rem 0.4rem;
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
        padding: 0.6rem 1rem;
        font-size: 0.8rem;
        color: #444;
        border-bottom: 1px solid #ddd;
        background: white;
        position: sticky;
        top: 0;
        z-index: 2;
    }
    .lh-row {
        display: flex;
        align-items: center;
        gap: 0.5rem;
    }
    .lh-row input[type="text"] {
        flex: 1;
        min-width: 0;
        padding: 0.35rem;
    }
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
        padding: 0.55rem 2rem;
        margin: 0 -2rem 0.6rem;
        border-bottom: 1px solid #eee;
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
        .burger {
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
        .drawer-backdrop {
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
        .body iframe {
            height: calc(100vh - 230px);
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
