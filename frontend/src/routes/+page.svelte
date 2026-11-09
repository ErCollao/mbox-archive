<script>
    import { onMount, tick } from 'svelte';

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
    let selectedEmail = null;
    let selectedEmailContent = "";
    let selectedThread = null;
    let threadMsgs = [];
    let msgData = {};
    let openMsgs = {};
    let openQuotes = {};
    let frameH = {};
    let mailPane;

    // Email frames arrive with their scripts stripped; the only JS running
    // inside is this height reporter, posted from an opaque sandbox origin.
    function onFrameMsg(e) {
        const d = e.data;
        if (d && d.t === 'mbox-h' && typeof d.h === 'number') {
            frameH = { ...frameH, [String(d.id)]: Math.min(20000, Math.max(80, d.h + 12)) };
            // Extend the settle pin only while one is already running, so a
            // later click inside a frame (e.g. "Show quoted text") never yanks.
            if (Date.now() < anchorUntil) {
                anchorUntil = Date.now() + 800;
            }
        }
    }

    // Keep the newest message pinned to the bottom of the pane while iframe
    // heights settle after opening a thread; release after a quiet period,
    // and instantly if the user scrolls themselves.
    let anchorTimer = null;
    let anchorUntil = 0;
    let programmatic = false;

    function reanchor() {
        anchorUntil = Date.now() + 800;
        if (anchorTimer) return;
        const step = () => {
            if (mailPane) {
                programmatic = true;
                mailPane.scrollTop = mailPane.scrollHeight;
                requestAnimationFrame(() => { programmatic = false; });
            }
            if (Date.now() < anchorUntil) {
                anchorTimer = setTimeout(step, 90);
            } else {
                anchorTimer = null;
            }
        };
        step();
    }

    function onPaneScroll() {
        if (!programmatic) {
            anchorUntil = 0;
            if (anchorTimer) {
                clearTimeout(anchorTimer);
                anchorTimer = null;
            }
        }
    }

    function frameDoc(html, id) {
        // Email CSS often pins html/body to height:100%, which makes
        // scrollHeight useless; measure the body's layout box instead.
        const reporter = '<scr' + 'ipt>(function(){' +
            'function p(){try{var b=document.body,d=document.documentElement;' +
            'var h=Math.max(b?b.getBoundingClientRect().height:0,b?b.offsetHeight:0,b?b.scrollHeight:0,d?d.scrollHeight:0);' +
            'parent.postMessage({t:"mbox-h",id:' + JSON.stringify(String(id)) + ',h:Math.round(h)},"*")}catch(e){}}' +
            'function hook(){for(var i=0;i<document.images.length;i++){var im=document.images[i];' +
            'if(!im._hb){im._hb=1;im.addEventListener("load",p);im.addEventListener("error",p);}}}' +
            'window.addEventListener("load",function(){hook();p();setTimeout(p,400);});' +
            'hook();' +
            'if(window.MutationObserver){new MutationObserver(hook).observe(document.documentElement,{childList:true,subtree:true});}' +
            'if(window.ResizeObserver){new ResizeObserver(p).observe(document.body||document.documentElement)}' +
            'document.addEventListener("click",function(){setTimeout(p,80);setTimeout(p,600)});' +
            '})();</scr' + 'ipt>';
        const base = '<base target="_blank">';
        if (/<\/body>/i.test(html)) return html.replace(/<\/body>/i, base + reporter + '</body>');
        return html + base + reporter;
    }

    let meta = null;
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
        if (isNaN(d.getTime())) return iso;
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
        if (b >= 1048576) return (b / 1048576).toFixed(1) + " MB";
        if (b >= 1024) return Math.round(b / 1024) + " KB";
        return b + " B";
    }

    function fmtCount(n) {
        return n >= 10000 ? (n / 1000).toFixed(0) + "k" : n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n);
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

    // Every section is single-select (click the active entry to clear it).
    // Fixed canonical Gmail labels live in Status; customs stay in Labels.
    const STATUS = ["Inbox", "Important", "Unread", "Opened", "Archived", "Starred", "Sent", "Drafts", "Spam", "Trash"];
    // Real Gmail tabs, fixed order (Promotions last) + official icon colors.
    const CAT_TABS = {
        "Category Personal": "#F9AB00",
        "Category Social": "#4285F4",
        "Category Updates": "#E37400",
        "Category Forums": "#9334E6",
        "Category Reservations": "#12B5CB",
        "Category Promotions": "#0B8043"
    };
    const CAT_ORDER = Object.keys(CAT_TABS);
    // Bills/Purchases/Travel etc. are "detected" categories: normal labels.
    const isCatTab = (n) => n in CAT_TABS;
    const dispFor = (n) => (isCatTab(n) ? n.slice("Category ".length) : n);

    function withAlpha(hex, a) {
        const r = parseInt(hex.slice(1, 3), 16);
        const g = parseInt(hex.slice(3, 5), 16);
        const b = parseInt(hex.slice(5, 7), 16);
        return `rgba(${r}, ${g}, ${b}, ${a})`;
    }

    // Darker variant for text/border contrast on light backgrounds.
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

    // Hierarchical custom labels ("Vault/Academia/…"): show top level only,
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
            // The exact path exists as a label itself (e.g. "Admin" plus "Admin/…").
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

    // Labels/Status toggle; top/Categories are single-select.
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

    onMount(() => {
        window.addEventListener('message', onFrameMsg);
        // Cleanup only ever runs in the browser (onMount is a no-op on the server).
        return () => window.removeEventListener('message', onFrameMsg);
    });

    onMount(async () => {
        await loadFacets();
        // Default view: the actual inbox = Inbox + Personal, when present.
        const names = labels.map(([n]) => n);
        if (names.includes("Category Personal")) sel = { ...sel, cat: "Category Personal" };
        loadInbox();
    });

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
        selectedEmail = null;
        selectedThread = null;
        threadMsgs = [];
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

    async function toggleMsg(m) {
        const id = m.id;
        if (openMsgs[id]) {
            openMsgs = { ...openMsgs, [id]: false };
            return;
        }
        openMsgs = { ...openMsgs, [id]: true };
        await ensureContent(m);
        const d = msgData[id];
        if (d && !d.meta) {
            try {
                const metaRes = await fetch(`${API_URL}/email/${id}/meta`);
                msgData = { ...msgData, [id]: { ...d, meta: metaRes.ok ? await metaRes.json() : null } };
            } catch (e) {
                msgData = { ...msgData, [id]: { ...d, meta: null } };
            }
        }
    }

    async function ensureContent(m) {
        const id = m.id;
        if (msgData[id] && msgData[id].content !== "") return;
        if (!msgData[id]) {
            msgData = { ...msgData, [id]: { loading: true, meta: null, content: "" } };
        }
        try {
            const content = await fetch(`${API_URL}/email/${id}`).then(r => r.text());
            msgData = { ...msgData, [id]: { ...msgData[id], loading: false, content } };
        } catch (e) {
            msgData = { ...msgData, [id]: { loading: false, meta: msgData[id].meta, content: "Error loading content" } };
        }
    }

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
        while (first > 0 && /^(on .+ wrote:|-+ ?original message ?-+|begin forwarded message|forwarded message|__.*__)$/i.test(lines[first - 1].trim())) {
            first--;
        }
        const head = lines.slice(0, first).join("\n");
        if (!head.trim()) return null;
        return { head, quote: lines.slice(first).join("\n") };
    }

    async function selectThread(t) {
        selectedThread = t;
        threadMsgs = [];
        msgData = {};
        openMsgs = {};
        meta = null;
        selectedEmail = null;
        selectedEmailContent = "";
        try {
            const res = await fetch(`${API_URL}/thread?tid=${encodeURIComponent(t.tid)}`);
            const d = await res.json();
            threadMsgs = d.messages;
            // Load everything in parallel, then show the complete conversation
            // in one paint — no staged pop-in, no late scroll.
            const pairs = await Promise.all(d.messages.map(async m => {
                const [content, metaRes] = await Promise.all([
                    fetch(`${API_URL}/email/${m.id}`).then(r => r.text()),
                    fetch(`${API_URL}/email/${m.id}/meta`).then(r => r.ok ? r.json() : null).catch(() => null)
                ]);
                return [m.id, { loading: false, meta: metaRes, content }];
            }));
            msgData = Object.fromEntries(pairs);
            openMsgs = Object.fromEntries(d.messages.map(m => [m.id, true]));
            await tick();
            reanchor();
        } catch (e) {
            console.error(e);
        }
    }
</script>

<svelte:head>
    <title>Mbox Archive</title>
</svelte:head>
<svelte:body class="app-shell" />

<div class="container" class:email-expanded={expanded} class:drawer-open={drawerOpen}>
    <aside class="pane pane-settings">
        <div class="title-row">
            <h1>Mbox Archive</h1>
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
                {#each groups as g, i (String(g.head))}
                    <div class="label-group" class:top={i === 0}>
                        {#if g.head}<div class="nav-head">{g.head}</div>{/if}
                        {#if g.field === 'other'}
                            {#each otherRows as r (r.full)}
                                {#if r.hasKids && r.self}
                                    <div class="tree-row" style="padding-left: {r.depth * 0.85}rem;">
                                        <button
                                            class="twist-btn"
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
                                {@const active = sel[g.field] === name}
                                <button
                                    class="label-item"
                                    class:on={active}
                                    style={active && isCatTab(name)
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
                    {#if loading}
                        Searching...
                    {:else if mode === 'search'}
                        {emails.length} match{emails.length === 1 ? '' : 'es'}{moreSearch ? ' (newest first)' : ''}
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
                        on:click={() => selectThread(t)}
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

    <section class="pane pane-email" bind:this={mailPane} on:scroll={onPaneScroll} class:has-email={selectedThread !== null}>
        {#if selectedThread}
            <div class="email-tools">
                <button class="back-btn" on:click={() => { selectedThread = null; threadMsgs = []; openMsgs = {}; msgData = {}; expanded = false; }}>Back</button>
                <button class="fs-toggle" on:click={() => expanded = !expanded}>
                    {expanded ? 'Restore' : 'Full screen'}
                </button>
            </div>
            <div class="email-view">
                <div class="header">
                    <h2>{selectedThread.subject || '(No Subject)'}</h2>
                    {#if threadMsgs.length > 1}
                        <p class="thread-count">{threadMsgs.length} messages · {fmtDateOnly(threadMsgs[0].date)} – {fmtDateOnly(threadMsgs[threadMsgs.length - 1].date)}</p>
                    {/if}
                </div>

                {#each threadMsgs as m (m.id)}
                    {@const mlabels = msgData[m.id]?.meta?.labels || []}
                    <div class="tmsg" class:open={openMsgs[m.id]}>
                        <button class="tmsg-row" on:click={() => toggleMsg(m)} title={m.from}>
                            <span class="chev">{openMsgs[m.id] ? '▾' : '▸'}</span>
                            <span class="who">{m.from.split(' <')[0]}</span>
                            {#if mlabels.length}
                                <span class="mini-labels">
                                    {#each mlabels.slice(0, 4) as l (l)}<span class="mini">{dispFor(l)}</span>{/each}
                                </span>
                            {/if}
                            {#if m.attachments}<span class="clip">📎</span>{/if}
                            <span class="when">{fmtDateOnly(m.date)}</span>
                        </button>
                        {#if openMsgs[m.id]}
                            <div class="tmsg-body">
                                {#if !msgData[m.id] || msgData[m.id].loading}
                                    <div class="loading">Loading message…</div>
                                {:else}
                                    {@const md = msgData[m.id]}
                                    {#if md.meta && md.meta.to.length}
                                        <p class="to-line" title={md.meta.to.join(', ')}><strong>To:</strong> {md.meta.to.map(t => t.split(' <')[0]).join(', ')}</p>
                                    {/if}
                                    {#if md.meta && md.meta.attachments.some(a => !a.is_inline)}
                                        <div class="attachments">
                                            <h3>
                                                Attachments
                                                {#if md.meta.inline_images}
                                                    <span class="note">({md.meta.inline_images} image{md.meta.inline_images === 1 ? '' : 's'} also shown inline)</span>
                                                {/if}
                                            </h3>
                                            {#each md.meta.attachments.filter(a => !a.is_inline) as a (a.part_index)}
                                                <a
                                                    class="download"
                                                    href="{API_URL}/attachment/{m.id}/{a.part_index}"
                                                >Download: {a.filename} ({fmtBytes(a.size)}, {a.content_type})</a>
                                            {/each}
                                        </div>
                                    {/if}
                                    {#if md.content.trim().startsWith('<')}
                                        <iframe
                                            class="msg-frame"
                                            srcdoc={frameDoc(md.content, m.id)}
                                            title="Email content"
                                            sandbox="allow-popups allow-scripts"
                                            scrolling="no"
                                            style="height: {frameH[m.id] || 320}px;"
                                        ></iframe>
                                    {:else if splitQuoted(md.content)}
                                        {@const q = splitQuoted(md.content)}
                                        <pre>{q.head}</pre>
                                        <button
                                            class="q-toggle"
                                            on:click={() => openQuotes = { ...openQuotes, [m.id]: !openQuotes[m.id] }}
                                        >{openQuotes[m.id] ? 'Hide quoted text' : `Show quoted text (${q.quote.split('\n').length} lines)`}</button>
                                        {#if openQuotes[m.id]}
                                            <pre class="quote">{q.quote}</pre>
                                        {/if}
                                    {:else}
                                        <pre>{md.content}</pre>
                                    {/if}
                                {/if}
                            </div>
                        {/if}
                    </div>
                {/each}
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
    .pane-settings h1 {
        padding: 0;
        margin: 0;
        font-size: 1.1rem;
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
    .title-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding: 1rem 1rem 0.4rem;
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
    .back-btn {
        margin-right: auto;
    }
    .bodies {
        font-size: 0.75rem;
        color: #555;
        display: flex;
        align-items: center;
        gap: 0.3rem;
        flex-basis: 100%;
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
    .label-group.top .lname {
        font-weight: 600;
    }
    .show-more .lname {
        color: #06c;
        font-size: 0.75rem;
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
    .twist-btn {
        border: none;
        background: transparent;
        color: #888;
        font-size: 0.7rem;
        padding: 0 0.3rem 0 0.55rem;
        cursor: pointer;
        align-self: center;
    }
    .twist-btn:hover {
        color: #333;
    }
    .label-item.inline {
        flex: 1;
        border-bottom: none;
        min-width: 0;
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
    }
    .label-row {
        display: flex;
        flex-wrap: wrap;
        gap: 0.3rem;
    }
    .header-labels {
        margin-top: 0.5rem;
    }
    .chip {
        font-size: 0.72rem;
        padding: 0.15rem 0.5rem;
        border: 1px solid #bbb;
        border-radius: 10px;
        background: white;
        cursor: pointer;
    }
    .chip.sm {
        cursor: default;
        background: #eef;
    }
    .chip .count {
        opacity: 0.6;
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
        box-shadow: 0 1px 3px rgba(0,0,0,0.4);
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
    .clear {
        font-size: 0.75rem;
        padding: 0.2rem 0.5rem;
        cursor: pointer;
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
    .thread-count {
        color: #777;
        font-size: 0.8rem;
        margin: 0.2rem 0 0;
    }
    .tmsg {
        border-top: 1px solid #eee;
    }
    .tmsg:last-child {
        border-bottom: 1px solid #eee;
    }
    .tmsg-row {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        width: 100%;
        padding: 0.4rem 0.2rem;
        background: transparent;
        border: none;
        cursor: pointer;
        font-size: 0.85rem;
        text-align: left;
    }
    .tmsg-row:hover {
        background: #f5f7fa;
    }
    .tmsg.open > .tmsg-row {
        background: #e8f0fe;
    }
    .chev {
        color: #888;
        width: 0.7rem;
        flex-shrink: 0;
    }
    .who {
        font-weight: 600;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        max-width: 40%;
    }
    .mini-labels {
        display: inline-flex;
        gap: 0.25rem;
        overflow: hidden;
    }
    .mini {
        font-size: 0.66rem;
        color: #666;
        background: #eceff1;
        border-radius: 7px;
        padding: 0.02rem 0.4rem;
        white-space: nowrap;
    }
    .when {
        margin-left: auto;
        color: #777;
        font-size: 0.75rem;
        white-space: nowrap;
    }
    .tmsg-body {
        padding: 0.1rem 0.2rem 0.9rem;
    }
    .to-line {
        margin: 0 0 0.4rem;
        font-size: 0.78rem;
        color: #555;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }
    .msg-frame {
        width: 100%;
        border: none;
        background: #fff;
        overflow: hidden;
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
    .email-view {
        padding: 1.5rem 2rem;
    }
    .email-tools {
        display: flex;
        justify-content: flex-end;
        padding: 0.6rem 1rem 0;
    }
    .email-tools button {
        font-size: 0.75rem;
        padding: 0.25rem 0.8rem;
        cursor: pointer;
        border: 1px solid #bbb;
        border-radius: 4px;
        background: white;
    }
    .email-view .header {
        border-bottom: 1px solid #eee;
        padding-bottom: 1rem;
        margin-bottom: 1rem;
    }
    .email-view pre {
        white-space: pre-wrap;
        font-family: monospace;
        background: #f9f9f9;
        padding: 1rem;
        border-radius: 4px;
        font-size: 0.9rem;
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
    .attachments {
        border: 1px solid #ddd;
        border-radius: 4px;
        background: #fafafa;
        padding: 0.5rem 1rem;
        margin-bottom: 1rem;
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
    @media (min-width: 901px) {
        .back-btn {
            display: none;
        }
    }
    @media (min-width: 641px) and (max-width: 900px) {
        .container {
            grid-template-columns: 270px 1fr;
        }
        .container.email-expanded {
            grid-template-columns: 270px 1fr;
        }
        .email-expanded .pane-settings,
        .email-expanded .pane-results {
            display: flex;
        }
    }
    @media (max-width: 900px) {
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
            padding: 0 1.4rem;
        }
        .fs-toggle {
            display: none;
        }
    }
    @media (max-width: 640px) {
        .container {
            grid-template-columns: 1fr;
            grid-template-rows: auto 1fr;
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
            padding: 0 1.4rem;
        }
        .fs-toggle {
            display: none;
        }
    }
</style>
