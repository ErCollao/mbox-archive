import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';

// Standalone of vite.config.js on purpose: the sveltekit() plugin drags in
// the whole SvelteKit runtime ($app/* modules, adapters); page components
// here need nothing from it beyond a plain svelte compile and the $lib alias.
export default defineConfig({
    plugins: [svelte({ hot: false })],
    resolve: {
        // Force browser resolution so svelte internals (onMount, render
        // callbacks) come from the client runtime, not the node/ssr stubs.
        conditions: ['browser', 'development|production'],
        alias: {
            $lib: fileURLToPath(new URL('./src/lib', import.meta.url)),
        },
    },
    test: {
        include: ['src/**/*.test.js'],
        environment: 'jsdom',
        // registers afterEach(cleanup) so components unmount between tests
        setupFiles: ['@testing-library/svelte/vitest'],
        server: {
            deps: { inline: [/svelte/] },
        },
    },
});
