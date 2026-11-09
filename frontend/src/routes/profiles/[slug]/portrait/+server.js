import { error } from '@sveltejs/kit';
import { env } from '$env/dynamic/private';

const BACKEND_URL = env.BACKEND_URL || 'http://backend:3000';

/** @type {import('./$types').RequestHandler} */
export async function GET({ params }) {
    let response;
    try {
        response = await fetch(`${BACKEND_URL}/profiles/${params.slug}/portrait`);
    } catch {
        throw error(502, 'Backend unavailable');
    }
    if (!response.ok) throw error(404, 'no portrait');
    return new Response(response.body, {
        headers: {
            'content-type': response.headers.get('content-type') || 'image/jpeg',
            'cache-control': 'public, max-age=300'
        }
    });
}
