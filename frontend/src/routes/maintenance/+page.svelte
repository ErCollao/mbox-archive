<script>
    import { onDestroy } from 'svelte';

    const API_URL = "/api";

    let status = null;
    let backupResult = null;
    let backingUp = false;
    let err = "";

    async function refresh() {
        try {
            const res = await fetch(`${API_URL}/status`);
            status = await res.json();
        } catch (e) {
            err = "Cannot reach the backend: " + e.message;
        }
    }
    refresh();
    const timer = setInterval(refresh, 1500);
    onDestroy(() => clearInterval(timer));

    async function runMerge() {
        err = "";
        try { await fetch(`${API_URL}/merge`, { method: 'POST' }); }
        catch (e) { err = "Merge failed to start: " + e.message; }
        refresh();
    }

    async function runScan() {
        err = "";
        try { await fetch(`${API_URL}/scan`, { method: 'POST' }); }
        catch (e) { err = "Scan failed to start: " + e.message; }
        refresh();
    }

    async function runBackup() {
        backingUp = true;
        err = "";
        backupResult = null;
        try {
            const res = await fetch(`${API_URL}/backup`, { method: 'POST' });
            backupResult = await res.json();
        } catch (e) {
            err = "Backup request failed: " + e.message;
        }
        backingUp = false;
        refresh();
    }

    function fmtBytes(b) {
        if (b == null) return "";
        if (b >= 1073741824) return (b / 1073741824).toFixed(2) + " GB";
        if (b >= 1048576) return (b / 1048576).toFixed(1) + " MB";
        return Math.round(b / 1024) + " KB";
    }

    function fmtNum(n) {
        return n == null ? "—" : n.toLocaleString();
    }

    function fmtDate(ts) {
        if (!ts) return "—";
        const d = new Date(ts);
        return isNaN(d.getTime()) ? "—" : d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
    }
</script>

<svelte:head>
    <title>Maintenance — Mbox Archive</title>
</svelte:head>

<div class="page">
    <header>
        <a class="back" href="/">← Back to archive</a>
        <h1>Maintenance &amp; backup</h1>
    </header>

    <section class="stats">
        {#if status}
            <div><strong>{fmtNum(status.total)}</strong><span>emails indexed</span></div>
            <div><strong>{fmtDate(status.oldest_ts)}</strong><span>oldest</span></div>
            <div><strong>{fmtDate(status.latest_ts)}</strong><span>latest</span></div>
            <div><strong class="src">{status.source || "nothing loaded"}</strong><span>current archive file</span></div>
        {:else}
            <div><strong>…</strong><span>loading status</span></div>
        {/if}
    </section>

    {#if status?.busy}
        <div class="busy">
            <span class="pulse"></span>
            A merge or scan is running. Leave this page open — the stats above refresh automatically when it finishes
            (large archives: expect minutes; progress is visible in the server logs).
        </div>
    {/if}
    {#if err}
        <div class="error">{err}</div>
    {/if}

    <section class="intro">
        <h2>How this backup works</h2>
        <p>
            This drive holds your <em>complete</em> email archive. Gmail is the source of truth only until you
            clean up there — afterwards, <strong>this copy is what preserves history</strong>. Merging never
            removes anything: it reads every <code>.mbox</code> in <code>data/</code> — including
            <code>merged.mbox</code> itself, which makes the archive cumulative — and for emails present in
            more than one file it keeps the copy from the newest one (so labels and read-status stay current).
            Emails you <em>deleted</em> in Gmail keep living on in the archive untouched. The result is written
            to a temp file and renamed only when the full copy succeeds — a failed merge can never damage your
            existing archive.
        </p>
    </section>

    <section class="step">
        <h2><span class="num">1</span> Bring the new Takeout in</h2>
        <p>
            Unzip the new Gmail Takeout and copy the <code>.mbox</code> file(s) into the <code>data/</code>
            folder on this drive — anywhere under it works (a <code>data/incoming/</code> subfolder keeps
            things tidy). The merged archive itself always lives at <code>data/merged.mbox</code>.
            You do <em>not</em> need to keep old raw exports around once they've been merged —
            <code>merged.mbox</code> already contains everything they ever contributed.
        </p>
    </section>

    <section class="step">
        <h2><span class="num">2</span> Merge</h2>
        <p>
            Rebuilds <code>merged.mbox</code> from every <code>.mbox</code> present, the existing archive
            included: union of all messages, newest file wins for duplicates. Needs roughly one
            archive-size of free space while running. Re-running it with nothing new is harmless — the
            merge is idempotent.
        </p>
        <button on:click={runMerge} disabled={status?.busy}>
            {status?.busy ? "Working…" : "Merge all mbox files"}
        </button>
    </section>

    <section class="step">
        <h2><span class="num">3</span> Re-scan</h2>
        <p>
            Rebuilds the search index over a <em>single</em> archive file: <code>merged.mbox</code> if it
            exists, otherwise the newest <code>.mbox</code> found (the server logs a warning when other
            files would be ignored — that means you wanted Merge first). Fast if the index already exists;
            a first pass on a multi-gigabyte archive takes a few minutes.
        </p>
        <button on:click={runScan} disabled={status?.busy}>
            {status?.busy ? "Working…" : "Re-scan the archive"}
        </button>
    </section>

    <section class="step">
        <h2><span class="num">4</span> Verify, then tidy</h2>
        <p>
            Check the stats at the top before trusting anything: <strong>emails indexed</strong> should be ≥
            last time, and <strong>latest</strong> should show the new export's date. Once the numbers look
            right, you can delete the raw takeout files from <code>data/</code> — their content now lives in
            <code>merged.mbox</code>, and the next merge will grow the archive from that.
        </p>
    </section>

    <section class="step">
        <h2><span class="num">5</span> Safe copy (redundancy)</h2>
        <p>
            Copies the current archive to the backup location as a timestamped, gzip-compressed file
            (<code>mbox-archive-&lt;date&gt;.mbox.gz</code> — takeout mboxes typically shrink to a third of
            their size). It is written to a temp file, then <em>verified by decompressing the whole backup
            and comparing byte counts</em> before being renamed into place — a partial or corrupt backup
            never shows up as a completed one. The progress bar below updates live.
            Point <code>BACKUP_DIR</code> in the compose file at a <em>different</em> physical disk than
            <code>DATA_DIR</code> for it to mean anything.
        </p>
        <button class="backup" on:click={runBackup} disabled={status?.busy || status?.backup?.running}>
            {status?.backup?.running ? "Backup running…" : "Safe-copy the archive now"}
        </button>
        {#if status?.backup?.running}
            <div class="prog">
                <progress max={status.backup.total || 1} value={status.backup.done}></progress>
                <span>
                    {status.backup.phase === "verifying" ? "verifying backup" : "compressing"} —
                    {status.backup.total ? Math.round(100 * status.backup.done / status.backup.total) : 0}%
                    ({fmtBytes(status.backup.done)} of {fmtBytes(status.backup.total)})
                </span>
            </div>
        {/if}
        {#if backupResult}
            <div class={backupResult.ok ? "result ok" : "result bad"}>
                {#if backupResult.ok}
                    ✓ {backupResult.message} {fmtBytes(backupResult.bytes)} in {backupResult.secs}s →
                    <code>{backupResult.target}</code>
                {:else}
                    ✗ {backupResult.message}
                {/if}
            </div>
        {/if}
    </section>

    <section class="step housekeeping">
        <h2>Golden rule</h2>
        <p>
            Never let a period of your history exist in only one place. After a Safe copy, the archive exists
            twice (drive + backup location). If the backup location is on the same physical drive, treat it as
            convenience, not redundancy. Old <code>mbox-archive-*</code> snapshots are just points in time —
            once a newer verified backup exists, you can delete the older ones (many people keep the newest
            plus one).
        </p>
    </section>
</div>

<style>
    .page {
        max-width: 720px;
        margin: 0 auto;
        padding: 2rem 1.5rem 4rem;
        font-family: system-ui, -apple-system, sans-serif;
        color: #222;
        line-height: 1.55;
    }
    :global(body) {
        margin: 0;
        background: #fafafa;
    }
    header {
        display: flex;
        align-items: baseline;
        justify-content: space-between;
        margin-bottom: 1rem;
    }
    h1 {
        font-size: 1.4rem;
        margin: 0;
    }
    .back {
        color: #1a73e8;
        text-decoration: none;
        font-size: 0.9rem;
    }
    .stats {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
        gap: 0.75rem;
        background: white;
        border: 1px solid #e0e0e0;
        border-radius: 8px;
        padding: 1rem;
        margin-bottom: 1rem;
    }
    .stats div {
        display: flex;
        flex-direction: column;
    }
    .stats strong {
        font-size: 1.1rem;
    }
    .stats .src {
        font-size: 0.8rem;
        word-break: break-all;
    }
    .stats span {
        font-size: 0.72rem;
        color: #777;
        text-transform: uppercase;
        letter-spacing: 0.03em;
    }
    .busy, .error {
        border-radius: 8px;
        padding: 0.75rem 1rem;
        margin-bottom: 1rem;
        font-size: 0.9rem;
    }
    .busy {
        background: #fef7e0;
        border: 1px solid #f5deb3;
    }
    .error {
        background: #fce8e6;
        border: 1px solid #f5c6c0;
    }
    .pulse {
        display: inline-block;
        width: 10px;
        height: 10px;
        border-radius: 50%;
        background: #e37400;
        margin-right: 0.5rem;
        animation: throb 1.2s ease-in-out infinite;
    }
    @keyframes throb {
        0%, 100% { opacity: 1; }
        50% { opacity: 0.3; }
    }
    .intro, .step {
        background: white;
        border: 1px solid #e0e0e0;
        border-radius: 8px;
        padding: 1rem 1.25rem;
        margin-bottom: 0.9rem;
    }
    h2 {
        font-size: 1rem;
        margin: 0 0 0.4rem;
        display: flex;
        align-items: center;
        gap: 0.6rem;
    }
    .num {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 1.5rem;
        height: 1.5rem;
        border-radius: 50%;
        background: #1a73e8;
        color: white;
        font-size: 0.85rem;
        flex-shrink: 0;
    }
    p {
        margin: 0.3rem 0 0.7rem;
        font-size: 0.92rem;
        color: #333;
    }
    button {
        padding: 0.55rem 1.1rem;
        font-size: 0.9rem;
        border: 1px solid #1a73e8;
        background: #1a73e8;
        color: white;
        border-radius: 6px;
        cursor: pointer;
    }
    button:disabled {
        background: #b9d1f5;
        border-color: #b9d1f5;
        cursor: wait;
    }
    button.backup {
        background: #0b8043;
        border-color: #0b8043;
    }
    button.backup:disabled {
        background: #a5d6b7;
        border-color: #a5d6b7;
    }
    .result {
        margin-top: 0.7rem;
        font-size: 0.85rem;
        padding: 0.5rem 0.8rem;
        border-radius: 6px;
    }
    .prog {
        display: flex;
        align-items: center;
        gap: 0.7rem;
        margin-top: 0.7rem;
        font-size: 0.85rem;
        color: #333;
    }
    .prog progress {
        flex: 1;
        height: 10px;
        accent-color: #1a73e8;
    }
    .result.ok {
        background: #e6f4ea;
        color: #1e4620;
    }
    .result.bad {
        background: #fce8e6;
        color: #7c2d12;
    }
    .housekeeping .num {
        display: none;
    }
    code {
        background: #f1f3f4;
        padding: 0.05rem 0.35rem;
        border-radius: 4px;
        font-size: 0.85em;
    }
</style>
