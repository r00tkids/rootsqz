import { defineConfig } from 'vite';
import glsl from 'vite-plugin-glsl';
import websqz from 'rollup-plugin-websqz';
import { importFromString } from 'module-from-string';

let glslPlugin = glsl({ minify: true });
export default defineConfig({
    root: '.',
    build: {
        outDir: 'dist',
        sourcemap: true,

        // Disable module preloading for better compression results
        modulePreload: false,
    },
    plugins: [
        websqz({
            fileTransforms: [
                {
                    include: [/\.glsl$/, /\.frag$/, /\.vert$/],
                    transform: async (ctx, id, content) => {
                        // The GLSL plugin doesn't have a simple interface to call it directly,
                        // so we use importFromString to leverage its existing Vite loader...
                        // THIS IS UNSAFE AND THE FILE LOADED CAN POTENTIALLY EXECUTE ARBITRARY CODE!
                        let glslPluginInstance = await glslPlugin;
                        let transformedSource = await glslPluginInstance.transform.handler.call(ctx, content.toString("utf-8"), id);
                        const shaderSource = await importFromString(transformedSource.code);
                        return {
                            content: Buffer.from(shaderSource.default, "utf-8"),
                            isCompressed: false,
                            isText: true,
                            fileExt: ".glsl"
                        };
                    }
                }
            ]
        }),
    ]
});