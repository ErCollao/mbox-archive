import { error } from '@sveltejs/kit';
import { env } from '$env/dynamic/private';

const BACKEND_URL = env.BACKEND_URL || 'http://backend:3000';

/** @type {import('./$types').RequestHandler} */
export async function GET({ params, url }) {
    const targetUrl = `${BACKEND_URL}/api/${params.path}${url.search}`;
    try {
        const response = await fetch(targetUrl);
        if (!response.ok) {
            throw error(response.status, await response.text());
        }
        const data = await response.text();
        return new Response(data, {
            headers: { 'content-type': response.headers.get('content-type') || 'application/json' }
        });
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
        return new Response(data, {
            headers: { 'content-type': response.headers.get('content-type') || 'application/json' }
        });
    } catch (e) {
        console.error('Proxy POST error:', e);
        throw error(500, 'Backend unavailable');
    }
}
