# rollup-plugin-websqz
![NPM Version](https://img.shields.io/npm/v/rollup-plugin-websqz)

Rollup / Vite plugin for using [websqz](https://github.com/r00tkids/websqz) to compress and bundle code and assets into one HTML file. This is intented for intros in the [demoscene](https://en.wikipedia.org/wiki/Demoscene) or size restricted JS challenges.

## Install
`npm i rollup-plugin-websqz`

## Usage
```js
// vite.config.js
import { defineConfig } from 'vite';
import websqz from 'rollup-plugin-websqz';

export default defineConfig({
  plugins: [websqz()]
});
```

See the [example](https://github.com/r00tkids/rollup-plugin-websqz/tree/main/example) for a working example with support for `vite-plugin-glsl`.

## Example Options
```js
websqz({
    /*
    Full path to the websqz executable.
    If null (default), the plugin uses the websqz executable
    installed when installing the npm package.
    Otherwise it will try to resolve websqz from your system PATH.
    */
    websqzPath: null,
    fileTransforms: [
        {
            include: /\.glsl$/,
            transform: async (ctx, id, content) => {
                return {
                    content: Buffer.from("Hello World", "utf-8"),
                    isCompressed: false,
                    isText: true,
                    fileExt: ".glsl" // Optional
                };
            }
        }
    ]
})
```