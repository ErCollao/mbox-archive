<script>
    let query = "";
    let emails = [];
    let selectedEmail = null;
    let selectedEmailContent = "";
    let loading = false;
    let scanning = false;
    let merging = false;

    // Use relative path since we proxy through SvelteKit server
    const API_URL = "/api";

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
        loading = true;
        emails = [];
        selectedEmail = null;
        try {
            const res = await fetch(`${API_URL}/search?q=${encodeURIComponent(query)}`);
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
        try {
            const res = await fetch(`${API_URL}/email/${email.id}`);
            selectedEmailContent = await res.text();
        } catch (e) {
            selectedEmailContent = "Error loading content";
        }
    }
</script>

<div class="container">
    <div class="sidebar">
        <h1>Mbox Browser</h1>
        
        <div class="actions">
            <button on:click={mergeMbox} disabled={merging}>
                {merging ? 'Merging...' : 'Merge Mbox Files'}
            </button>
            <button on:click={scanMbox} disabled={scanning}>
                {scanning ? 'Scanning...' : 'Scan /data'}
            </button>
        </div>

        <div class="search-box">
            <input 
                type="text" 
                bind:value={query} 
                placeholder="Search subject, body..." 
                on:keydown={(e) => e.key === 'Enter' && search()}
            />
            <button on:click={search}>Search</button>
        </div>

        <div class="email-list">
            {#if loading}
                <div class="loading">Searching...</div>
            {:else if emails.length === 0}
                <div class="empty">No results</div>
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
                            <span class="date">{email.date}</span>
                        </div>
                    </div>
                {/each}
            {/if}
        </div>
    </div>

    <div class="main">
        {#if selectedEmail}
            <div class="email-view">
                <div class="header">
                    <h2>{selectedEmail.subject}</h2>
                    <p><strong>From:</strong> {selectedEmail.from}</p>
                    <p><strong>Date:</strong> {selectedEmail.date}</p>
                </div>
                <div class="body">
                    {#if selectedEmailContent.trim().startsWith('<')}
                        <iframe 
                            srcdoc={selectedEmailContent} 
                            title="Email content"
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
    }
    .actions button {
        padding: 0.5rem;
        cursor: pointer;
    }
    .search-box {
        padding: 0.5rem 1rem;
        display: flex;
        gap: 0.5rem;
    }
    .search-box input {
        flex: 1;
        padding: 0.4rem;
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
</style>
