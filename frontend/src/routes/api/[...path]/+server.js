import { error } from '@sveltejs/kit';
import { env } from '$env/dynamic/private';

const BACKEND_URL = env.BACKEND_URL || 'http://backend:3000';

/**
 * Forward the backend response, preserving security headers
 * (CSP sandbox / nosniff) alongside the content type.
 */
function forwarded(response) {
    const headers = new Headers();
    for (const key of ['content-type', 'content-disposition', 'content-security-policy', 'x-content-type-options']) {
        const value = response.headers.get(key);
        if (value) headers.set(key, value);
    }
    return headers;
}

const TEXT_TYPES = /json|text|html|xml|javascript/;

/** @type {import('./$types').RequestHandler} */
export async function GET({ params, url }) {
    const targetUrl = `${BACKEND_URL}/api/${params.path}${url.search}`;
    try {
        const response = await fetch(targetUrl);
        if (!response.ok) {
            throw error(response.status, await response.text());
        }
        const headers = forwarded(response);
        const contentType = (response.headers.get('content-type') || '').toLowerCase();
        // Binary payloads (attachment images/files) must be streamed, not re-encoded as text.
        if (!TEXT_TYPES.test(contentType) && response.body) {
            return new Response(response.body, { headers });
        }
        const data = await response.text();
        if (!headers.has('content-type')) headers.set('content-type', 'application/json');
        return new Response(data, { headers });
    } catch (e) {
        console.error('Proxy GET error:', e);
        throw error(500, 'Backend unavailable');
    }
}

/** @type {import('./$types').RequestHandler} */
export async function POST({ params, request }) {
    const targetUrl = `${BACKEND_URL}/api/${params.path}`;
    try {
        const response = await fetch(targetUrl, {
            method: 'POST',
            body: await request.text(),
            headers: { 'content-type': 'application/json' }
        });
        const data = await response.text();
        return new Response(data, { headers: forwarded(response) });
    } catch (e) {
        console.error('Proxy POST error:', e);
        throw error(500, 'Backend unavailable');
    }
}
