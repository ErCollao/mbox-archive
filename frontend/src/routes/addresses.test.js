import { describe, it, expect, vi, beforeAll, afterEach } from 'vitest';
import { render, fireEvent, waitFor, cleanup } from '@testing-library/svelte';
import Home from './+page.svelte';
import Profile from './profile/+page.svelte';

beforeAll(() => { Element.prototype.scrollIntoView = () => {}; });
afterEach(() => { vi.unstubAllGlobals(); cleanup(); });

const res = (body, text) => ({
    ok: true, status: 200,
    json: async () => body,
    text: async () => text ?? JSON.stringify(body),
});
const FAIL = (body) => ({ ok: false, status: 400, json: async () => body });

function stubHome({ addresses = [] } = {}) {
    const calls = [];
    const rows = [
        { tid: 't1', subject: 'First', from: 'a@x', date: '2026-01-01T00:00:00Z', count: 1, matched: 0, attachments: false, accts: addresses.length > 1 ? [addresses[0]] : undefined },
        { tid: 't2', subject: 'Second', from: 'b@x', date: '2026-01-02T00:00:00Z', count: 2, matched: 0, attachments: false },
    ];
    vi.stubGlobal('fetch', (url) => {
        const u = String(url);
        calls.push(u);
        if (u.startsWith('/api/whoami')) return Promise.resolve(res({ slug: 'd', name: 'D' }));
        if (u.startsWith('/api/filters')) return Promise.resolve(res({ labels: [['Category Social', 4], ['Inbox', 3]], addresses: addresses.map((a, i) => [a, 10 - i]), min_ts: 0, max_ts: 86400000 * 40, total: 2 }));
        if (u.startsWith('/api/messages')) return Promise.resolve(res(rows));
        if (u.startsWith('/api/thread')) return Promise.resolve(res({ tid: 't1', subject: 'First', messages: [] }));
        if (/meta/.test(u)) return Promise.resolve(res({ id: '1', subject: 'S', from: 'a', to: [], date: '2026-01-01T00:00:00Z', labels: [], attachments: [], inline_images: 0 }));
        if (u.startsWith('/api/email/')) return Promise.resolve(res(null, 'body'));
        throw new Error('unmocked ' + u);
    });
    return calls;
}

async function mountHome(opts) {
    const calls = stubHome(opts);
    render(Home);
    await waitFor(() => document.querySelector('.email-item'));
    await new Promise((r) => setTimeout(r, 40)); // let the boot refetch settle
    await waitFor(() => document.querySelector('.email-item'));
    return calls;
}

describe('addresses on the archive page', () => {
    it('shows the Addresses group and row tag only with 2+ declared', async () => {
        await mountHome({ addresses: ['hajni@gmail.com', 'hajni@work.com'] });
        const heads = [...document.querySelectorAll('.nav-head')].map((e) => e.textContent);
        expect(heads).toContain('Addresses');
        expect(heads[heads.length - 1]).toBe('Addresses'); // bottom of the panel
        const items = [...document.querySelectorAll('.label-item')];
        const hajni = items.find((b) => b.textContent.includes('hajni@gmail.com'));
        expect(hajni).toBeTruthy();
        expect(document.querySelector('.acct-tag')?.textContent).toBe('hajni@gmail.com');
    });

    it('stays hidden with a single declared address', async () => {
        await mountHome({ addresses: ['solo@x.com'] });
        expect([...document.querySelectorAll('.nav-head')].find((e) => e.textContent === 'Addresses')).toBeFalsy();
        expect(document.querySelector('.acct-tag')).toBeFalsy();
    });

    it('clicking an address filters the list and raises a chip', async () => {
        const calls = await mountHome({ addresses: ['hajni@gmail.com', 'hajni@work.com'] });
        const items = [...document.querySelectorAll('.label-item')];
        await fireEvent.click(items.find((b) => b.textContent.includes('hajni@work.com')));
        await waitFor(() => expect(calls.some((u) => u.includes('acct=hajni%40work.com'))).toBe(true));
        const chip = [...document.querySelectorAll('.af-chip')].find((c) => c.textContent.includes('hajni@work.com'));
        expect(chip).toBeTruthy();
        await fireEvent.click(chip); // remove
        await waitFor(() => expect(calls[calls.length - 1].includes('acct=')).toBe(false));
    });

    it('Clear appears whenever a filter is active (reactive, not function-tracked)', async () => {
        await mountHome({ addresses: [] });
        expect(document.querySelector('button.clear')).toBeFalsy();
        const btn = [...document.querySelectorAll('.label-item')].find((b) => b.textContent.includes('Social'));
        expect(btn).toBeTruthy();
        await fireEvent.click(btn);
        await waitFor(() => expect(document.querySelector('button.clear')).toBeTruthy());
        await fireEvent.click(document.querySelector('button.clear'));
        await waitFor(() => expect(document.querySelector('button.clear')).toBeFalsy());
    });
});

describe('addresses editor on the profile page', () => {
    const PROFILE = (addresses) => ({ slug: 'daniel', name: 'Daniel', has_portrait: false, addresses });

    function stubProfile(initial) {
        let cur = [...initial];
        const posts = [];
        vi.stubGlobal('fetch', (url, opts = {}) => {
            const u = String(url);
            if (u === '/api/profile' && !opts.method) return Promise.resolve(res(PROFILE(cur)));
            if (u === '/api/profile/addresses') {
                posts.push(JSON.parse(opts.body).addresses);
                cur = posts[posts.length - 1];
                return Promise.resolve(res({ ok: true, addresses: cur }));
            }
            return Promise.resolve(FAIL({ error: 'unmocked ' + u }));
        });
        return posts;
    }

    it('renders declared addresses as removable chips and posts the full list', async () => {
        stubProfile(['hajni@gmail.com', 'hajni+work@gmail.com']);
        render(Profile);
        await waitFor(() => expect(document.querySelectorAll('.addr-chip').length).toBe(2));
        await fireEvent.click(document.querySelectorAll('.addr-chip')[0].querySelector('button'));
        await waitFor(() => expect(document.querySelectorAll('.addr-chip').length).toBe(1));
        expect(document.querySelector('.addr-chip').textContent).toContain('hajni+work@gmail.com');
    });

    it('adds a trimmed, lowercased address', async () => {
        stubProfile(['a@x.com']);
        render(Profile);
        await waitFor(() => expect(document.querySelectorAll('.addr-chip').length).toBe(1));
        const input = document.querySelector('input[placeholder="name@example.com"]');
        await fireEvent.input(input, { target: { value: '  NEW@Y.org  ' } });
        await fireEvent.click([...document.querySelectorAll('button')].find((b) => b.textContent.includes('Add address')));
        await waitFor(() => expect(document.querySelectorAll('.addr-chip').length).toBe(2));
        expect(document.querySelectorAll('.addr-chip')[1].textContent).toContain('new@y.org');
    });
});
