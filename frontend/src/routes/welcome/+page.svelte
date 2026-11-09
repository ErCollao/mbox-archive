<script>
    // Profile gate: real roster from /api/profiles, real login via /api/login.
    // Stage lives in the URL (?p=<slug>) so browser back/forward work.

    import { onMount } from 'svelte';
    import { page } from '$app/stores';

    const TINTS = ["#1a73e8", "#0b8043", "#d93025", "#8430ce", "#e37400", "#12a4af"];

    let profiles = [];

    $: slug = $page.url.searchParams.get("p");
    $: chosen = profiles.find(p => p.slug === slug) || null;

    let pw = "";
    let showPw = false;
    let shake = false;
    $: (slug, pw = "", showPw = false, shake = false);

    onMount(async () => {
        try {
            const res = await fetch('/api/profiles');
            profiles = await res.json();
        } catch { /* roster unreachable: chooser stays empty */ }
    });

    function tint(i) { return TINTS[i % TINTS.length]; }

    function submit() {
        const body = JSON.stringify({ slug: chosen.slug, password: pw });
        fetch('/api/login', {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body
        })
            .then(r => {
                if (r.ok) window.location.href = "/";
                else throw new Error("bad");
            })
            .catch(() => {
                shake = true;
                setTimeout(() => { shake = false; pw = ""; }, 450);
            });
    }
</script>

<svelte:head>
    <title>Mbox Archive — sign in</title>
</svelte:head>

<div class="screen">
    {#if !chosen}
        <div class="panel">
            <div class="brand"><span class="brand-dot"></span> Mbox Archive</div>
            <h1>Who's browsing?</h1>
            <p class="sub">Each profile sees only its own archive.</p>
            <div class="cards">
                {#each profiles as p, i (p.slug)}
                    <a class="card" href="?p={p.slug}">
                        <span class="picwrap" style="--tint:{tint(i)}">
                            {#if p.has_portrait}
                                <img src="/profiles/{p.slug}/portrait" alt={p.name} />
                            {:else}
                                <span class="mono">{p.name.charAt(0)}</span>
                            {/if}
                        </span>
                        <span class="pname">{p.name}</span>
                    </a>
                {/each}
            </div>
        </div>
    {:else}
        <div class="panel {shake ? 'shake' : ''}">
            <button class="backchip" title="Back" on:click={() => history.back()}>
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                    <line x1="19" y1="12" x2="5" y2="12"/><polyline points="12 19 5 12 12 5"/>
                </svg>
            </button>
            <span class="picwrap big" style="--tint:{tint(profiles.indexOf(chosen))}">
                {#if chosen.has_portrait}
                    <img src="/profiles/{chosen.slug}/portrait" alt={chosen.name} />
                {:else}
                    <span class="mono">{chosen.name.charAt(0)}</span>
                {/if}
            </span>
            <h1>{chosen.name}</h1>
            <p class="sub">Password</p>
            <form class="pwgroup" on:submit|preventDefault={submit}>
                {#if showPw}
                    <input type="text" bind:value={pw} placeholder="password" autocomplete="current-password" autofocus />
                {:else}
                    <input type="password" bind:value={pw} placeholder="password" autocomplete="current-password" autofocus />
                {/if}
                <button type="button" class="seg" title={showPw ? 'Hide password' : 'Show password'} on:click={() => showPw = !showPw}>
                    {#if showPw}
                        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                            <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94"/>
                            <path d="M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19"/>
                            <path d="M14.12 14.12a3 3 0 1 1-4.24-4.24"/>
                            <line x1="2" y1="2" x2="22" y2="22"/>
                        </svg>
                    {:else}
                        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                            <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/>
                            <circle cx="12" cy="12" r="3"/>
                        </svg>
                    {/if}
                </button>
                <button type="submit" class="seg go-seg" title="Enter" disabled={!pw}>
                    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">
                        <line x1="5" y1="12" x2="19" y2="12"/><polyline points="12 5 19 12 12 19"/>
                    </svg>
                </button>
            </form>
        </div>
    {/if}
</div>

<style>
    :global(body) {
        margin: 0;
        background: #f7f7f7; /* the app's own field colour */
    }
    .screen {
        min-height: 100vh;
        box-sizing: border-box;
        padding: 1.5rem;
        font-family: system-ui, -apple-system, sans-serif;
        color: #202124;
        display: flex;
        align-items: center;
        justify-content: center;
    }

    .panel {
        position: relative;
        background: #fff;
        border: 1px solid #e0e0e0;
        border-radius: 10px;
        padding: 1.9rem 2rem;
        width: min(420px, 100%);
        display: flex;
        flex-direction: column;
        align-items: center;
        text-align: center;
        box-shadow: 0 2px 10px rgba(0, 0, 0, 0.04);
    }

    .brand {
        font-size: .68rem;
        letter-spacing: .09em;
        text-transform: uppercase;
        color: #999;
        display: flex;
        align-items: center;
        gap: .45rem;
        justify-content: center;
        margin-bottom: 1.2rem;
    }
    .brand-dot { width: 8px; height: 8px; border-radius: 2px; background: #1a73e8; }
    h1 { font-size: 1.25rem; margin: 0 0 .25rem; font-weight: 600; }
    .sub { color: #666; margin: 0 0 1.4rem; font-size: .85rem; }

    .cards { display: flex; gap: 1.1rem; justify-content: center; flex-wrap: wrap; }
    .card {
        width: 150px;
        padding: .5rem .5rem .85rem;
        border-radius: 10px;
        border: 1px solid transparent;
        background: transparent;
        cursor: pointer;
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: .65rem;
        transition: background .12s ease, border-color .12s ease;
        color: #202124;
        text-decoration: none;
    }
    .card:hover { background: #f5f7fa; border-color: #e0e0e0; }
    .picwrap {
        width: 104px;
        height: 104px;
        border-radius: 50%;
        overflow: hidden;
        display: flex;
        align-items: center;
        justify-content: center;
        background: var(--tint, #ccc);
        border: 2px solid #fff;
        box-shadow: 0 0 0 1px #e0e0e0, 0 3px 10px rgba(0, 0, 0, 0.10);
    }
    .picwrap img { width: 100%; height: 100%; object-fit: cover; display: block; }
    .picwrap.big { width: 88px; height: 88px; margin-bottom: .9rem; }
    .mono { color: white; font-size: 2.4rem; font-weight: 600; }
    .picwrap.big .mono { font-size: 2rem; }
    .pname { font-weight: 600; font-size: .95rem; }

    .shake { animation: shake .4s; }
    @keyframes shake { 0%, 100% { transform: none } 25% { transform: translateX(-6px) } 75% { transform: translateX(6px) } }

    .backchip {
        position: absolute;
        top: 1.1rem;
        left: 1.1rem;
        width: 34px;
        height: 34px;
        border-radius: 50%;
        border: 1px solid #e0e0e0;
        background: #fff;
        color: #80868b;
        cursor: pointer;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: 0;
        transition: border-color .12s ease, color .12s ease;
    }
    .backchip:hover { border-color: #1a73e8; color: #1a73e8; }

    .pwgroup {
        display: flex;
        align-items: stretch;
        width: 100%;
        max-width: 320px;
        border: 1px solid #d0d3d8;
        border-radius: 8px;
        background: #fff;
        overflow: hidden;
    }
    .pwgroup:focus-within { border-color: #1a73e8; box-shadow: 0 0 0 3px rgba(26, 115, 232, 0.16); }
    .pwgroup input {
        flex: 1;
        min-width: 0;
        border: none;
        outline: none;
        background: transparent;
        padding: .65rem .8rem;
        font-size: .95rem;
        font-family: inherit;
        color: #202124;
    }
    .seg {
        border: none;
        border-left: 1px solid #e8eaed;
        background: transparent;
        color: #80868b;
        cursor: pointer;
        padding: 0 .7rem;
        display: flex;
        align-items: center;
    }
    .seg:hover { background: #f1f3f4; color: #202124; }
    .go-seg { color: #1a73e8; }
    .go-seg:disabled { color: #c0c4c9; cursor: default; background: transparent; }
</style>
