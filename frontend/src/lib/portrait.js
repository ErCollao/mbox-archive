// Client-side portrait normalization: the chooser renders a square, so we
// center-crop to a fixed size before upload instead of shipping a 12 MB
// original and letting CSS fake the framing.

export const PORTRAIT_SIZE = 512;

/// Centered square over an img of w×h: the largest square, equally cropped.
export function squareCrop(w, h) {
    const side = Math.min(w, h);
    return { sx: (w - side) / 2, sy: (h - side) / 2, side };
}

/// Draw `img` square onto a fresh canvas and return a JPEG data URL.
/// Transparent sources get a white matte. `createCanvas` is injectable so
/// the draw call sequence can be asserted without a real canvas backend.
export function cropToJpeg(img, {
    size = PORTRAIT_SIZE,
    quality = 0.9,
    createCanvas = () => document.createElement('canvas'),
} = {}) {
    const c = createCanvas();
    c.width = size;
    c.height = size;
    const ctx = c.getContext('2d');
    ctx.fillStyle = '#fff';
    ctx.fillRect(0, 0, size, size);
    const { sx, sy, side } = squareCrop(img.naturalWidth, img.naturalHeight);
    ctx.drawImage(img, sx, sy, side, side, 0, 0, size, size);
    return c.toDataURL('image/jpeg', quality);
}
