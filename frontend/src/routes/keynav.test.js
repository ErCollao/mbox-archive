import { describe, it, expect, vi, beforeAll, afterEach } from 'vitest';
import { render, fireEvent, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import Home from './+page.svelte';

beforeAll(() => {
    Element.prototype.scrollIntoView = () => {};
});

const MESSAGES = [
    { tid: 't1', subject: 'First', from: 'a@x', date: '2026-01-01T00:00:00Z', count: 1, matched: 0, attachments: false },
    { tid: 't2', subject: 'Second', from: 'b@x', date: '2026-01-02T00:00:00Z', count: 2, matched: 0, attachments: false },
];
const THREAD = { tid: 't2', subject: 'Second', messages: [{ id: '1', subject: 'S', from: 'b@x', date: '2026-01-02T00:00:00Z', snippet: '', attachments: false }, { id: '2', subject: 'S', from: 'c@x', date: '2026-01-03T00:00:00Z', snippet: '', attachments: false }] };

function mockFetch(big = false) {
    const res = (body, text) => ({
        ok: true, status: 200,
        json: async () => body,
        text: async () => text ?? JSON.stringify(body),
    });
    const row = (i) => ({ tid: 't' + i, subject: 'M' + i, from: 'a@x', date: '2026-01-01T00:00:00Z', count: 1, matched: 0, attachments: false });
    const calls = [];
    const f = (url, opts) => {
        calls.push(url);
        const u = String(url);
        if (u.startsWith('/api/whoami')) return Promise.resolve(res({ slug: 'd', name: 'D' }));
        if (u.startsWith('/api/filters')) return Promise.resolve(res({ labels: [['Category Personal', 5], ['Category Social', 7], ['Inbox', 3]], min_ts: 0, max_ts: 86400000 * 40, total: 2 }));
        if (u.startsWith('/api/messages')) {
            if (big && u.includes('offset=100')) return Promise.resolve(res([row(100), row(101), row(102)]));
            if (big) return Promise.resolve(res(Array.from({ length: 100 }, (_, i) => row(i))));
            return Promise.resolve(res(MESSAGES));
        }
        if (u.startsWith('/api/search')) return Promise.resolve(res([MESSAGES[0]]));
        if (u.startsWith('/api/thread')) return Promise.resolve(res(THREAD));
        if (/\/api\/email\/[^/]+\/meta/.test(u)) return Promise.resolve(res({ id: '1', subject: 'S', from: 'b@x', to: ['me@x'], date: '2026-01-02T00:00:00Z', labels: [], attachments: [], inline_images: 0 }));
        if (u.startsWith('/api/email/')) return Promise.resolve(res(null, 'plain body'));
        throw new Error('unmocked ' + u);
    };
    vi.stubGlobal('fetch', f);
    return calls;
}

async function key(target, k, opts = {}) {
    await fireEvent.keyDown(target, { key: k, ...opts });
    await tick();
}

async function mount(big = false) {
    const calls = mockFetch(big);
    render(Home);
    await waitFor(() => document.querySelector('.email-item'));
    // settle the transient facet-triggered refetch that briefly empties the list
    await new Promise((r) => setTimeout(r, 50));
    await waitFor(() => document.querySelector('.email-item'));
    await tick();
    return { calls };
}

afterEach(() => vi.unstubAllGlobals());

describe('vim key navigation', () => {
    it('l enters the list pane, j/k move the cursor, Enter opens but focus stays', async () => {
        const { calls } = await mount();

        await key(window, 'l');
        expect(document.querySelector('.pane-results.kbd-focus')).toBeTruthy();
        expect(document.querySelector('.email-item.kcursor')).toBeFalsy(); // -1 until first j

        await key(window, 'j');
        let rows = document.querySelectorAll('.email-item');
        expect(rows[0].classList.contains('kcursor')).toBe(true);

        await key(window, 'j');
        rows = document.querySelectorAll('.email-item');
        expect(rows[1].classList.contains('kcursor')).toBe(true);
        expect(rows[0].classList.contains('kcursor')).toBe(false);

        await key(window, 'k');
        expect(document.querySelectorAll('.email-item')[0].classList.contains('kcursor')).toBe(true);

        await key(window, 'Enter');
        await waitFor(() => expect(calls.some((u) => String(u).startsWith('/api/thread?tid=t1'))).toBe(true));
        // selection must NOT yank focus into the email pane
        expect(document.querySelector('.pane-results.kbd-focus')).toBeTruthy();
        expect(document.querySelector('.pane-email.kbd-focus')).toBeFalsy();
    });

    it('arrow keys alias h/j/k/l', async () => {
        await mount();
        await key(window, 'ArrowDown'); // cold start owns the list
        expect(document.querySelector('.pane-results.kbd-focus')).toBeTruthy();
        expect(document.querySelectorAll('.email-item')[0].classList.contains('kcursor')).toBe(true);
        await key(window, 'ArrowDown');
        expect(document.querySelectorAll('.email-item')[1].classList.contains('kcursor')).toBe(true);
        await key(window, 'ArrowRight');
        expect(document.querySelector('.pane-email.kbd-focus')).toBeTruthy();
        await key(window, 'ArrowLeft');
        expect(document.querySelector('.pane-results.kbd-focus')).toBeTruthy();
    });

    it('Escape outside fields clears pane focus', async () => {
        await mount();
        await key(window, 'l');
        expect(document.querySelector('.pane-results.kbd-focus')).toBeTruthy();
        await key(window, 'Escape');
        expect(document.querySelector('.pane-results.kbd-focus')).toBeFalsy();
    });

    it('j/k in the email pane step through thread messages', async () => {
        const { calls } = await mount();
        await key(window, 'l'); await key(window, 'j'); await key(window, 'Enter');
        await key(window, 'l'); // focus deliberately moves right, not by opening
        // opening a thread lands on the newest message ("2")
        await waitFor(() => expect(calls.some((u) => String(u).startsWith('/api/email/2'))).toBe(true));
        calls.length = 0;
        await key(window, 'k');
        await waitFor(() => expect(calls.some((u) => String(u) === '/api/email/1')).toBe(true));
    });

    it('h and l stop at the edges instead of rotating', async () => {
        await mount();
        await key(window, 'h');
        expect(document.querySelector('.pane-settings.kbd-focus')).toBeTruthy();
        await key(window, 'h'); // already leftmost
        expect(document.querySelector('.pane-settings.kbd-focus')).toBeTruthy();
        expect(document.querySelector('.pane-results.kbd-focus')).toBeFalsy();
        await key(window, 'l'); await key(window, 'l'); await key(window, 'l');
        expect(document.querySelector('.pane-email.kbd-focus')).toBeTruthy();
        await key(window, 'l'); // already rightmost
        expect(document.querySelector('.pane-email.kbd-focus')).toBeTruthy();
        expect(document.querySelectorAll('.kbd-focus').length).toBe(1);
    });

    it('a fresh search resets the row cursor', async () => {
        await mount();
        await key(window, 'l');
        await key(window, 'j');
        expect(document.querySelector('.email-item.kcursor')).toBeTruthy();
        const input = document.querySelector('.searchbox input');
        await fireEvent.input(input, { target: { value: 'report' } });
        await fireEvent.keyDown(input, { key: 'Enter' });
        await waitFor(() => expect(document.querySelectorAll('.email-item').length).toBeGreaterThan(0));
        expect(document.querySelector('.email-item.kcursor')).toBeFalsy(); // no stale selection
        await key(window, 'j');
        expect(document.querySelector('.email-item.kcursor')).toBeTruthy(); // and j starts fresh at top
    });

    it('/ focuses the search field; keys stay letters while typing', async () => {
        await mount();
        await key(window, '/');
        const input = document.querySelector('.searchbox input');
        expect(document.activeElement).toBe(input);

        // with focus in the field, even an activated list pane must not move
        await key(window, 'l');
        await fireEvent.keyDown(input, { key: 'j' }); // target=input, bubbles to window handler
        await tick();
        expect(document.querySelector('.email-item.kcursor')).toBeFalsy();

        // Escape leaves the field; navigation keys regain their meaning
        await fireEvent.keyDown(input, { key: 'Escape' });
        await tick();
        expect(document.activeElement).not.toBe(input);
        await key(window, 'j');
        expect(document.querySelector('.email-item.kcursor')).toBeTruthy();
    });

    it('a mouse click opens a thread without grabbing keyboard focus', async () => {
        await mount();
        await waitFor(() => {
            const row = document.querySelectorAll('.email-item')[0];
            if (!row) throw new Error('no rows yet');
            fireEvent.click(row);
        });
        await waitFor(() => expect(document.querySelector('.back-btn')).toBeTruthy());
        expect(document.querySelector('.kbd-focus')).toBeFalsy(); // mouse mode: no pane ring
        await fireEvent.focus(document.querySelector('.email-list')); // click-bubbled focus must not claim
        expect(document.querySelector('.kbd-focus')).toBeFalsy();
    });

    it('opening a thread pushes history so Back can exit it', async () => {
        await mount();
        await key(window, 'l'); await key(window, 'j'); await key(window, 'Enter');
        await waitFor(() => expect(document.querySelector('.msg-head')).toBeTruthy());
        // t1 was pushed on open; the mock thread identifies itself as t2 and
        // the message step replaces ?t — either way a t= must be in history
        expect(window.location.search).toContain('t=t2');
        // emulate the browser landing back on the pre-thread entry
        window.history.replaceState(null, '', '/');
        window.dispatchEvent(new PopStateEvent('popstate', { state: null }));
        await waitFor(() => expect(document.querySelector('.msg-head')).toBeFalsy());
    });

    it('G at the bottom loads the next page and the cursor rides along', async () => {
        await mount(true); // 100 rows, then 3 more
        await key(window, 'l');
        await key(window, 'G'); // jump to end (row 99)
        let rows = document.querySelectorAll('.email-item');
        expect(rows.length).toBe(100);
        expect(rows[99].classList.contains('kcursor')).toBe(true);

        await key(window, 'G'); // at end -> load more
        await waitFor(() => expect(document.querySelectorAll('.email-item').length).toBe(103));
        rows = document.querySelectorAll('.email-item');
        expect(rows[102].classList.contains('kcursor')).toBe(true);

        await key(window, 'j'); // no more pages: stays put, no crash
        rows = document.querySelectorAll('.email-item');
        expect(rows.length).toBe(103);
        expect(rows[102].classList.contains('kcursor')).toBe(true);
    });

    it('j at the bottom also triggers the next page', async () => {
        await mount(true);
        await key(window, 'l');
        await key(window, 'G'); // end of first page
        await key(window, 'j'); // overflow -> load
        await waitFor(() => expect(document.querySelectorAll('.email-item').length).toBe(103));
        expect(document.querySelectorAll('.email-item')[102].classList.contains('kcursor')).toBe(true);
    });

    it('? opens the shortcut help, swallows nav keys, and closes on ?/Escape/Close', async () => {
        await mount();
        await key(window, '?');
        expect(document.querySelector('.help-card')).toBeTruthy();
        await key(window, 'j'); // help eats navigation while open
        expect(document.querySelector('.help-card')).toBeTruthy();
        expect(document.querySelector('.email-item.kcursor')).toBeFalsy();
        await key(window, 'Escape');
        expect(document.querySelector('.help-card')).toBeFalsy();
        await key(window, '?');
        expect(document.querySelector('.help-card')).toBeTruthy();
        await key(window, '?');
        expect(document.querySelector('.help-card')).toBeFalsy();
    });

    it('the bottom-left key opens and the Close button shuts the help', async () => {
        await mount();
        await fireEvent.click(document.querySelector('.help-key'));
        expect(document.querySelector('.help-card')).toBeTruthy();
        await fireEvent.click(document.querySelector('.help-card button'));
        expect(document.querySelector('.help-card')).toBeFalsy();
    });

    it('space activates the focused left-pane filter', async () => {
        const { calls } = await mount();
        await key(window, 'h'); // null -> left, cursor paints on item 0
        await key(window, 'j'); // onto Category Social (Personal auto-selects at boot)
        const before = calls.length;
        await key(window, ' ');
        await waitFor(() => expect(document.querySelector('.label-item.on')).toBeTruthy());
        expect(document.querySelectorAll('.label-item')[1].classList.contains('on')).toBe(true);
        await waitFor(() => expect(calls.length).toBeGreaterThan(before)); // refetch fired
    });

    it('left cursor hides when navigating away and remembers its spot', async () => {
        await mount();
        await key(window, 'h');
        await key(window, 'j'); // to item 1
        expect(document.querySelectorAll('.label-item')[1].classList.contains('kcursor')).toBe(true);
        await key(window, 'l'); // to the list
        expect(document.querySelector('.label-item.kcursor')).toBeFalsy();
        await key(window, 'h'); // back left
        expect(document.querySelectorAll('.label-item')[1].classList.contains('kcursor')).toBe(true);
    });

    it('Escape hides all highlights but keeps cursor positions', async () => {
        await mount();
        await key(window, 'h');
        await key(window, 'j');
        await key(window, 'Escape');
        expect(document.querySelector('.kbd-focus')).toBeFalsy();
        expect(document.querySelector('.kcursor')).toBeFalsy();
        await key(window, 'h');
        expect(document.querySelectorAll('.label-item')[1].classList.contains('kcursor')).toBe(true);
    });

    it('G jumps to the end of the list, gg back to the top', async () => {
        await mount();
        await key(window, 'l');
        await key(window, 'G');
        let rows = document.querySelectorAll('.email-item');
        expect(rows[rows.length - 1].classList.contains('kcursor')).toBe(true);
        await key(window, 'g');
        await key(window, 'g');
        rows = document.querySelectorAll('.email-item');
        expect(rows[0].classList.contains('kcursor')).toBe(true);
    });

    it('the email pane gets the fixed frame; others get none', async () => {
        await mount();
        await key(window, 'l');
        expect(document.querySelector('.kbd-frame.show')).toBeFalsy();
        await key(window, 'l');
        expect(document.querySelector('.kbd-frame.show')).toBeTruthy();
        await key(window, 'h');
        expect(document.querySelector('.kbd-frame.show')).toBeFalsy();
    });

    it('Escape closes an open thread from the email pane', async () => {
        await mount();
        await key(window, 'l'); await key(window, 'j'); await key(window, 'Enter');
        await waitFor(() => expect(document.querySelector('.msg-head')).toBeTruthy());
        await key(window, 'l'); // email pane
        expect(document.querySelector('.kbd-frame.show')).toBeTruthy();
        await key(window, 'Escape');
        expect(document.querySelector('.msg-head')).toBeFalsy(); // left the reading
        expect(document.querySelector('.kbd-frame.show')).toBeFalsy();
        expect(document.querySelector('.kbd-focus')).toBeFalsy();  // and hid highlights
        await key(window, 'l');                                    // back into the list
        expect(document.querySelector('.email-item.kcursor')).toBeTruthy(); // cursor kept
    });

    it('f and Enter toggle full screen in the email pane', async () => {
        await mount();
        await key(window, 'l'); await key(window, 'j'); await key(window, 'Enter'); // open
        await key(window, 'l'); // focus the email pane
        await key(window, 'f');
        expect(document.querySelector('.container.email-expanded')).toBeTruthy();
        await key(window, 'Enter');
        expect(document.querySelector('.container.email-expanded')).toBeFalsy();
    });

    it('the list is an accessible listbox driven by the same cursor', async () => {
        const { calls } = await mount();
        const list = document.querySelector('.email-list');
        expect(list.getAttribute('role')).toBe('listbox');
        expect(list.getAttribute('tabindex')).toBe('0');
        expect(document.querySelectorAll('[role="option"]').length).toBe(2);
        await key(document.body, 'Shift'); // keyboard interaction precedes keyboard focus (must traverse document capture, like real keydowns)
        await fireEvent.focus(list); // focusing with kb intent claims the pane, like l would
        expect(document.querySelector('.pane-results.kbd-focus')).toBeTruthy();
        await key(list, 'ArrowDown');
        expect(list.getAttribute('aria-activedescendant')).toBe('email-row-0');
        expect(document.querySelectorAll('.email-item')[0].classList.contains('kcursor')).toBe(true);
        await fireEvent.keyDown(list, { key: 'Enter' }); // local open, single path
        await waitFor(() => expect(calls.some((u) => String(u).startsWith('/api/thread'))).toBe(true));
    });
});
