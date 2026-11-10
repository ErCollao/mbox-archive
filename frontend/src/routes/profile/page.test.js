import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import Profile from './+page.svelte';

const ok = (body) => ({ ok: true, status: 200, json: async () => body });
const fail = (body) => ({ ok: false, status: 400, json: async () => body });

function mockFetch(handler) {
    const fetchMock = vi.fn((url, opts = {}) =>
        Promise.resolve(handler(url, opts.method || 'GET', opts.body))
    );
    vi.stubGlobal('fetch', fetchMock);
    return fetchMock;
}

afterEach(() => vi.unstubAllGlobals());

async function mountProfile(profileHandler) {
    const fetchMock = mockFetch((url, method, body) => {
        if (url === '/api/profile' && method === 'GET') return ok({ slug: 'daniel', name: 'Daniel', has_portrait: false });
        return profileHandler(url, method, body);
    });
    render(Profile);
    await screen.findByText("Daniel's profile");
    return fetchMock;
}

describe('profile page', () => {
    it('saves a trimmed display name and updates the header', async () => {
        const fetchMock = await mountProfile((url, method, body) => {
            if (url === '/api/profile/name')
                return ok({ name: JSON.parse(body).name.trim(), ok: true });
            return fail({ error: 'unexpected ' + url });
        });

        const input = screen.getByPlaceholderText('Display name');
        const button = screen.getByRole('button', { name: 'Save name' });
        expect(button.disabled).toBe(true); // unchanged name must be inert

        await fireEvent.input(input, { target: { value: '  Dan  ' } });
        expect(button.disabled).toBe(false);
        await fireEvent.click(button);

        await waitFor(() => expect(fetchMock).toHaveBeenCalledWith(
            '/api/profile/name',
            expect.objectContaining({ method: 'POST', body: JSON.stringify({ name: '  Dan  ' }) })
        ));
        await screen.findByText('Name updated.');
        await screen.findByText("Dan's profile");
    });

    it('keeps the old name and surfaces the error when the API rejects', async () => {
        await mountProfile((url, method) => {
            if (url === '/api/profile/name') return fail({ error: 'name needs 1–80 characters' });
            return fail({ error: 'unexpected ' + url });
        });

        await fireEvent.input(screen.getByPlaceholderText('Display name'), { target: { value: '' } });
        expect(screen.getByRole('button', { name: 'Save name' }).disabled).toBe(true);

        await fireEvent.input(screen.getByPlaceholderText('Display name'), { target: { value: '  Dan  ' } });
        await fireEvent.click(screen.getByRole('button', { name: 'Save name' }));
        await screen.findByText('name needs 1–80 characters');
        await screen.findByText("Daniel's profile");
    });

    it('renders the photo section and the rename hint', async () => {
        await mountProfile(() => fail({ error: 'unused' }));
        expect(screen.getByText('Photo')).toBeTruthy();
        expect(screen.getByText(/signs nobody out/)).toBeTruthy();
    });
});
