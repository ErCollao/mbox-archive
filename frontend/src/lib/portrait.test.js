import { describe, it, expect } from 'vitest';
import { cropToJpeg, squareCrop, PORTRAIT_SIZE } from './portrait.js';

describe('squareCrop', () => {
    it('crops landscape horizontally, centered', () => {
        expect(squareCrop(1000, 600)).toEqual({ sx: 200, sy: 0, side: 600 });
    });
    it('crops portrait vertically, centered', () => {
        expect(squareCrop(600, 1000)).toEqual({ sx: 0, sy: 200, side: 600 });
    });
    it('leaves a square untouched', () => {
        expect(squareCrop(500, 500)).toEqual({ sx: 0, sy: 0, side: 500 });
    });
});

describe('cropToJpeg', () => {
    const fakeCanvas = (calls) => ({
        width: 0,
        height: 0,
        getContext: () => ({
            set fillStyle(v) { calls.fillStyle = v; },
            fillRect: (...a) => calls.fillRect = a,
            drawImage: (...a) => calls.drawImage = a,
        }),
        toDataURL: (type, quality) => {
            calls.toDataURL = [type, quality];
            return 'data:image/jpeg;base64,FAKE';
        },
    });

    it('white-mattes, center-crops, and re-exports as 512px JPEG', () => {
        const calls = {};
        const img = { naturalWidth: 4000, naturalHeight: 3000 };
        const out = cropToJpeg(img, { createCanvas: () => fakeCanvas(calls) });

        expect(out).toBe('data:image/jpeg;base64,FAKE');
        expect(calls.fillStyle).toBe('#fff');
        expect(calls.fillRect).toEqual([0, 0, PORTRAIT_SIZE, PORTRAIT_SIZE]);
        expect(calls.drawImage).toEqual([img, 500, 0, 3000, 3000, 0, 0, PORTRAIT_SIZE, PORTRAIT_SIZE]);
        expect(calls.toDataURL).toEqual(['image/jpeg', 0.9]);
    });

    it('honours a custom size and quality', () => {
        const calls = {};
        cropToJpeg({ naturalWidth: 100, naturalHeight: 100 }, {
            size: 64, quality: 0.5, createCanvas: () => fakeCanvas(calls),
        });
        expect(calls.fillRect).toEqual([0, 0, 64, 64]);
        expect(calls.toDataURL).toEqual(['image/jpeg', 0.5]);
    });
});
