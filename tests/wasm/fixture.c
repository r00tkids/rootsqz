// Test fixture for the wasm support of the compression report.
// Rebuild with tests/wasm/build.sh.

static float table[16] = {0.1f, 0.25f, 0.5f, 0.75f, 1.f, 1.5f, 2.f, 3.f,
                          4.f, 6.f, 8.f, 12.f, 16.f, 24.f, 32.f, 48.f};

static float wave(float t) {
    float x = t - (int)t;
    return x < 0.5f ? 4.f * x - 1.f : 3.f - 4.f * x;
}

__attribute__((export_name("render")))
void render(float *out, int n, float pitch) {
    for (int i = 0; i < n; i++) {
        float env = table[i & 15];
        out[i] = wave(i * pitch) * env;
    }
}

__attribute__((export_name("mix")))
float mix(float a, float b, float t) {
    return a + (b - a) * t;
}
