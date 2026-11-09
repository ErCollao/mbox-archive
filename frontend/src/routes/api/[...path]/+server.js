import { error } from '@sveltejs/kit';
import { env } from '$env/dynamic/private';

const BACKEND_URL = env.BACKEND_URL || 'http://backend:3000';

/**
 * Forward the backend response, preserving security headers
 * (CSP sandbox / nosniff) alongside the content type — and the
 * session cookie the backend sets on login.
 */
function forwarded(response) {
    const headers = new Headers();
    for (const key of ['content-type', 'content-disposition', 'content-security-policy', 'x-content-type-options', 'cache-control']) {
        const value = response.headers.get(key);
        if (value) headers.set(key, value);
    }
    // Multi-value safe: set-cookie must not collapse.
    for (const c of response.headers.getSetCookie?.() ?? []) {
        headers.append('set-cookie', c);
    }
    return headers;
}

const TEXT_TYPES = /json|text|html|xml|javascript/;

/** @type {import('./$types').RequestHandler} */
export async function GET({ params, url, request }) {
    const targetUrl = `${BACKEND_URL}/api/${params.path}${url.search}`;
    let response;
    try {
        response = await fetch(targetUrl, {
            headers: { cookie: request.headers.get('cookie') || '' }
        });
    } catch (e) {
        console.error('Proxy GET error:', e);
        throw error(502, 'Backend unavailable');
    }
    const headers = forwarded(response);
    const contentType = (response.headers.get('content-type') || '').toLowerCase();
    // Binary payloads (attachment images/files) must be streamed, not re-encoded as text.
    if (!TEXT_TYPES.test(contentType) && response.body) {
        return new Response(response.body, { status: response.status, headers });
    }
    const data = await response.text();
    if (!headers.has('content-type')) headers.set('content-type', 'application/json');
    // Never let browsers cache live API data (indexes change behind the scenes).
    if (!headers.has('cache-control')) headers.set('cache-control', 'no-store');
    return new Response(data, { status: response.status, headers });
}

/** @type {import('./$types').RequestHandler} */
export async function POST({ params, request }) {
    const targetUrl = `${BACKEND_URL}/api/${params.path}`;
    let response;
    try {
        response = await fetch(targetUrl, {
            method: 'POST',
            body: await request.text(),
            headers: {
                'content-type': 'application/json',
                cookie: request.headers.get('cookie') || ''
            }
        });
    } catch (e) {
        console.error('Proxy POST error:', e);
        throw error(502, 'Backend unavailable');
    }
    const data = await response.text();
    return new Response(data, { status: response.status, headers: forwarded(response) });
}

/** @type {import('./$types').RequestHandler} */
export async function DELETE({ params, request }) {
    const targetUrl = `${BACKEND_URL}/api/${params.path}${new URL(request.url).search}`;
    let response;
    try {
        response = await fetch(targetUrl, {
            method: 'DELETE',
            headers: { cookie: request.headers.get('cookie') || '' }
        });
    } catch (e) {
        console.error('Proxy DELETE error:', e);
        throw error(502, 'Backend unavailable');
    }
    const data = await response.text();
    return new Response(data, { status: response.status, headers: forwarded(response) });
}
