<script>
    import { onMount } from 'svelte';
    import { cropToJpeg } from '$lib/portrait.js';

    let me = null;         // { slug, name, has_portrait }
    let bust = 0;          // cache-buster for the portrait image
    let err = "";
    let note = "";

    // name form
    let displayName = "";
    let savingName = false;

    // password form
    let cur = "", next = "", again = "";
    let saving = false;

    async function load() {
        const res = await fetch('/api/profile');
        if (res.status === 401) { window.location.href = '/welcome'; return; }
        me = await res.json();
        displayName = me.name;
        bust = Date.now();
    }
    onMount(load);

    function flash(message, isError) {
        err = isError ? message : "";
        note = isError ? "" : message;
        setTimeout(() => { err = note = ""; }, 5000);
    }

    async function pickPhoto(e) {
        const file = e.target.files?.[0];
        e.target.value = ""; // allow re-picking the same file
        if (!file) return;
        if (file.size > 12_000_000) return flash("Image is larger than 12 MB.", true);
        if (!["image/jpeg", "image/png", "image/webp"].includes(file.type))
            return flash("Only JPEG, PNG, or WebP images work.", true);
        const data = await new Promise((resolve, reject) => {
            const r = new FileReader();
            r.onload = () => resolve(r.result);
            r.onerror = () => reject(new Error("read failed"));
            r.readAsDataURL(file);
        }).catch(() => null);
        if (!data) return flash("Could not read that file.", true);
        const img = await new Promise((resolve, reject) => {
            const i = new Image();
            i.onload = () => resolve(i);
            i.onerror = () => reject(new Error("decode failed"));
            i.src = data;
        }).catch(() => null);
        if (!img) return flash("That file isn't a valid image.", true);
        const jpeg = cropToJpeg(img);
        const res = await fetch('/api/profile/portrait', {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({ data: jpeg })
        });
        const out = await res.json();
        if (!res.ok) return flash(out.error || "Upload failed.", true);
        me = { ...me, has_portrait: true };
        bust = Date.now();
        flash("Photo updated.");
    }

    async function removePhoto() {
        await fetch('/api/profile/portrait', { method: 'DELETE' });
        me = { ...me, has_portrait: false };
        flash("Photo removed.");
    }

    async function saveName() {
        err = note = "";
        savingName = true;
        try {
            const res = await fetch('/api/profile/name', {
                method: 'POST',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ name: displayName })
            });
            const out = await res.json();
            if (!res.ok) return flash(out.error || "Could not save the name.", true);
            me = { ...me, name: out.name };
            displayName = out.name;
            flash("Name updated.");
        } finally {
            savingName = false;
        }
    }

    async function changePassword() {
        err = note = "";
        if (next !== again) return flash("The two new passwords don't match.", true);
        saving = true;
        try {
            const res = await fetch('/api/profile/password', {
                method: 'POST',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ current: cur, new: next })
            });
            const out = await res.json();
            if (!res.ok) return flash(out.error || "Could not change the password.", true);
            cur = next = again = "";
            flash("Password changed.");
        } finally {
            saving = false;
        }
    }

    async function lock() {
        await fetch('/api/logout', { method: 'POST' });
        window.location.href = '/welcome';
    }
</script>

<svelte:head>
    <title>Profile — Mbox Archive</title>
</svelte:head>

{#if me}
    <div class="page">
        <header>
            <a class="back" href="/">← Back to archive</a>
            <h1>{me.name}'s profile</h1>
        </header>

        {#if err || note}
            <div class="msg" class:bad={err} class:good={note}>{err || note}</div>
        {/if}

        <section>
            <h2>Name</h2>
            <p class="hint">How you're labelled on the sign-in screen and in the archive corner. Changing it signs nobody out.</p>
            <div class="fields">
                <input
                    type="text"
                    bind:value={displayName}
                    placeholder="Display name"
                    maxlength="80"
                    on:keydown={(e) => e.key === 'Enter' && saveName()}
                />
            </div>
            <button
                class="btn"
                on:click={saveName}
                disabled={savingName || !displayName.trim() || displayName.trim() === me.name}
            >
                {savingName ? "Saving…" : "Save name"}
            </button>
        </section>

        <section>
            <h2>Photo</h2>
            <p class="hint">Shown to the other profiles on the sign-in screen — that's how you tell each other apart.</p>
            <div class="photo-row">
                <span class="picwrap">
                    {#if me.has_portrait}
                        <img src="/profiles/{me.slug}/portrait?v={bust}" alt="your portrait" />
                    {:else}
                        <span class="mono">{me.name.charAt(0)}</span>
                    {/if}
                </span>
                <div class="photo-actions">
                    <label class="btn">
                        Upload photo
                        <input type="file" accept="image/jpeg,image/png,image/webp" on:change={pickPhoto} hidden />
                    </label>
                    {#if me.has_portrait}
                        <button class="btn quiet" on:click={removePhoto}>Remove</button>
                    {/if}
                </div>
            </div>
        </section>

        <section>
            <h2>Password</h2>
            <p class="hint">Six characters at least. Stored only as a bcrypt hash in <code>profiles/{me.slug}/profile.json</code>.</p>
            <div class="fields">
                <input type="password" bind:value={cur} placeholder="Current password" autocomplete="current-password" />
                <input type="password" bind:value={next} placeholder="New password" autocomplete="new-password" />
                <input type="password" bind:value={again} placeholder="Repeat new password" autocomplete="new-password" />
            </div>
            <button class="btn" on:click={changePassword} disabled={saving || !cur || !next}>
                {saving ? "Saving…" : "Change password"}
            </button>
        </section>

        <section>
            <h2>This device</h2>
            <p class="hint">The sign-in lasts a year. Lock it when you're done.</p>
            <button class="btn quiet" on:click={lock}>Lock & switch profile</button>
        </section>
    </div>
{/if}

<style>
    .page {
        max-width: 560px;
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
        margin-bottom: 1.4rem;
    }
    h1 { font-size: 1.4rem; margin: 0; }
    .back { color: #1a73e8; text-decoration: none; font-size: .9rem; }
    section {
        background: white;
        border: 1px solid #e0e0e0;
        border-radius: 8px;
        padding: 1.2rem 1.3rem;
        margin-bottom: 1rem;
    }
    h2 { font-size: 1rem; margin: 0 0 .2rem; }
    .hint { font-size: .82rem; color: #777; margin: 0 0 1rem; }
    code { background: #f1f3f4; padding: .05rem .35rem; border-radius: 4px; font-size: .85em; }
    .msg {
        border-radius: 8px;
        padding: .7rem 1rem;
        margin-bottom: 1rem;
        font-size: .9rem;
    }
    .msg.bad { background: #fce8e6; border: 1px solid #f5b8b1; color: #a02010; }
    .msg.good { background: #e6f4ea; border: 1px solid #b7dfc0; color: #1e6b34; }
    .photo-row { display: flex; align-items: center; gap: 1.1rem; }
    .picwrap {
        width: 88px; height: 88px; border-radius: 50%;
        overflow: hidden; flex-shrink: 0;
        display: flex; align-items: center; justify-content: center;
        background: #1a73e8;
        border: 2px solid #fff;
        box-shadow: 0 0 0 1px #e0e0e0, 0 3px 10px rgba(0,0,0,.10);
    }
    .picwrap img { width: 100%; height: 100%; object-fit: cover; display: block; }
    .mono { color: white; font-size: 2rem; font-weight: 600; }
    .photo-actions { display: flex; flex-direction: column; gap: .5rem; align-items: flex-start; }
    .btn {
        display: inline-block;
        background: #1a73e8; color: white; border: none; border-radius: 6px;
        padding: .5rem 1rem; font-size: .88rem; font-family: inherit;
        cursor: pointer; text-decoration: none;
    }
    .btn:hover { background: #1765cc; }
    .btn:disabled { background: #a9c7ef; cursor: default; }
    .btn.quiet { background: white; color: #1a73e8; border: 1px solid #d0d3d8; }
    .btn.quiet:hover { background: #f5f7fa; }
    .fields { display: flex; flex-direction: column; gap: .6rem; margin-bottom: 1rem; }
    .fields input {
        border: 1px solid #d0d3d8; border-radius: 8px; padding: .65rem .8rem;
        font-size: .95rem; font-family: inherit; color: #202124; background: white;
    }
    .fields input:focus { outline: none; border-color: #1a73e8; box-shadow: 0 0 0 3px rgba(26,115,232,.16); }
</style>
