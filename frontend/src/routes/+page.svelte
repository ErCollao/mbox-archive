<script>
    import { onMount } from 'svelte';

    let query = "";
    let emails = [];
    let mode = "inbox"; // "inbox" | "search"
    let searchBodies = false;
    let hasAttachments = false;
    let selectedLabels = [];
    let labels = [];
    let facetMin = 0;
    let facetMax = 0;
    let sliderLo = 0;
    let sliderHi = 0;
    let afterTs = null;
    let beforeTs = null;
    let hasMore = false;
    let offset = 0;
    let selectedEmail = null;
    let selectedEmailContent = "";
    let meta = null;
    let loading = false;
    let scanning = false;
    let merging = false;

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
        return selectedLabels.length > 0 || afterTs !== null || beforeTs !== null || hasAttachments;
    }

    function facetParams() {
        const p = new URLSearchParams();
        if (selectedLabels.length) p.set("label", selectedLabels.join(","));
        if (afterTs !== null) p.set("after", isoDate(afterTs));
        if (beforeTs !== null) p.set("before", isoDate(beforeTs));
        if (hasAttachments) p.set("attachment", "1");
        return p;
    }

    function applyFilters() {
        if (query.trim()) search();
        else loadInbox();
    }

    function toggleLabel(name) {
        // Gmail-style: one label at a time; clicking the active one clears.
        selectedLabels = selectedLabels.length === 1 && selectedLabels[0] === name
            ? []
            : [name];
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
        selectedLabels = [];
        afterTs = null;
        beforeTs = null;
        hasAttachments = false;
        sliderLo = 0;
        sliderHi = totalDays();
        query = "";
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

    onMount(async () => {
        await Promise.all([loadFacets(), loadInbox()]);
    });

    async function scanMbox() {
        scanning = true;
        try {
            await fetch(`${API_URL}/scan`, { method: 'POST' });
            alert("Scan started! It may take a while for large files. Try searching in a minute.");
        } catch (e) {
            alert("Error starting scan: " + e.message);
        } finally {
            scanning = false;
        }
    }

    async function mergeMbox() {
        merging = true;
        try {
            await fetch(`${API_URL}/merge`, { method: 'POST' });
            alert("Merge process started! This will combine all .mbox files in /data into merged.mbox and deduplicate messages. Once finished, run Scan.");
        } catch (e) {
            alert("Error starting merge: " + e.message);
        } finally {
            merging = false;
        }
    }

    async function search() {
        const trimmed = query.trim();
        if (!trimmed) {
            return loadInbox();
        }
        loading = true;
        mode = "search";
        emails = [];
        selectedEmail = null;
        try {
            const p = facetParams();
            p.set("q", trimmed);
            if (searchBodies) p.set("body", "1");
            const res = await fetch(`${API_URL}/search?${p}`);
            emails = await res.json();
        } catch (e) {
            console.error(e);
        } finally {
            loading = false;
        }
    }

    async function selectEmail(email) {
        selectedEmail = email;
        selectedEmailContent = "Loading...";
        meta = null;
        try {
            const [m, c] = await Promise.all([
                fetch(`${API_URL}/email/${email.id}/meta`).then(r => r.ok ? r.json() : null),
                fetch(`${API_URL}/email/${email.id}`).then(r => r.text())
            ]);
            meta = m;
            selectedEmailContent = c;
        } catch (e) {
            selectedEmailContent = "Error loading content";
        }
    }
</script>

<div class="container">
    <div class="sidebar">
        <h1>Mbox Browser</h1>

        <div class="search-box">
            <input
                type="text"
                bind:value={query}
                placeholder="Search subject/sender, or from:dave subject:x"
                on:keydown={(e) => e.key === 'Enter' && search()}
            />
            <button on:click={search}>Search</button>
            <label class="bodies">
                <input type="checkbox" bind:checked={searchBodies} on:change={applyFilters} />
                Include message bodies (full-archive scan, takes minutes)
            </label>
        </div>

        {#if labels.length > 0}
            <div class="labels">
                {#each labels as [name, count] (name)}
                    <button
                        class="label-item"
                        class:on={selectedLabels.length === 1 && selectedLabels[0] === name}
                        on:click={() => toggleLabel(name)}
                    >
                        <span class="lname">{name}</span>
                        <span class="count">{fmtCount(count)}</span>
                    </button>
                {/each}
            </div>
        {/if}

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
                <button class="clear" on:click={clearFilters}>Clear filters</button>
            {/if}
        </div>

        <div class="email-list">
            {#if loading}
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
                {#each emails as email}
                    <div
                        class="email-item"
                        class:selected={selectedEmail && selectedEmail.id === email.id}
                        on:click={() => selectEmail(email)}
                    >
                        <div class="subject" title={email.subject}>{email.subject || '(No Subject)'}</div>
                        <div class="meta">
                            <span class="from" title={email.from}>{email.from}</span>
                            <span class="date">{fmtDate(email.date)}</span>
                        </div>
                    </div>
                {/each}
                {#if mode === 'inbox' && hasMore}
                    <div class="load-more">
                        <button on:click={() => loadInbox(false)}>Load 100 more</button>
                    </div>
                {/if}
            {/if}
        </div>

        <div class="actions">
            <button on:click={mergeMbox} disabled={merging}>
                {merging ? 'Merging...' : 'Merge Mbox Files'}
            </button>
            <button on:click={scanMbox} disabled={scanning}>
                {scanning ? 'Scanning...' : 'Re-scan /data'}
            </button>
        </div>
    </div>

    <div class="main">
        {#if selectedEmail}
            <div class="email-view">
                <div class="header">
                    <h2>{selectedEmail.subject}</h2>
                    <p><strong>From:</strong> {selectedEmail.from}</p>
                    {#if meta && meta.to.length}
                        <p><strong>To:</strong> {meta.to.join(', ')}</p>
                    {/if}
                    <p><strong>Date:</strong> {fmtDate(selectedEmail.date)}</p>
                    {#if meta && meta.labels.length}
                        <div class="label-row header-labels">
                            {#each meta.labels as l (l)}
                                <span class="chip sm">{l}</span>
                            {/each}
                        </div>
                    {/if}
                </div>

                {#if meta && meta.attachments.some(a => !a.is_inline)}
                    <div class="attachments">
                        <h3>
                            Attachments
                            {#if meta.inline_images}
                                <span class="note">({meta.inline_images} image{meta.inline_images === 1 ? '' : 's'} also shown inline)</span>
                            {/if}
                        </h3>
                        {#each meta.attachments.filter(a => !a.is_inline) as a (a.part_index)}
                            <a
                                class="download"
                                href="{API_URL}/attachment/{selectedEmail.id}/{a.part_index}"
                            >Download: {a.filename} ({fmtBytes(a.size)}, {a.content_type})</a>
                        {/each}
                    </div>
                {/if}

                <div class="body">
                    {#if selectedEmailContent.trim().startsWith('<')}
                        <iframe
                            srcdoc={selectedEmailContent}
                            title="Email content"
                            sandbox="allow-popups"
                            style="width: 100%; height: 600px; border: 1px solid #ddd; background: white;"
                        ></iframe>
                    {:else}
                        <pre>{selectedEmailContent}</pre>
                    {/if}
                </div>
            </div>
        {:else}
            <div class="placeholder">
                Select an email to view
            </div>
        {/if}
    </div>
</div>

<style>
    :global(body) {
        margin: 0;
        font-family: system-ui, -apple-system, sans-serif;
        height: 100vh;
        overflow: hidden;
    }
    .container {
        display: flex;
        height: 100vh;
    }
    .sidebar {
        width: 350px;
        background: #f7f7f7;
        border-right: 1px solid #ddd;
        display: flex;
        flex-direction: column;
    }
    .sidebar h1 {
        padding: 1rem;
        margin: 0;
        font-size: 1.2rem;
    }
    .actions {
        padding: 0.5rem 1rem;
        display: flex;
        flex-direction: column;
        gap: 0.5rem;
        border-top: 1px solid #ddd;
    }
    .actions button {
        padding: 0.5rem;
        cursor: pointer;
    }
    .search-box {
        padding: 0.5rem 1rem;
        display: flex;
        flex-wrap: wrap;
        gap: 0.5rem;
    }
    .search-box input[type="text"] {
        flex: 1;
        padding: 0.4rem;
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
        margin: 0 1rem 0.4rem;
        max-height: 170px;
        overflow-y: auto;
        border: 1px solid #e2e2e2;
        border-radius: 4px;
        background: white;
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
        background: #333;
        color: white;
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
    .chip.on {
        background: #333;
        color: white;
        border-color: #333;
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
    .main {
        flex: 1;
        overflow-y: auto;
        padding: 2rem;
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
</style>
