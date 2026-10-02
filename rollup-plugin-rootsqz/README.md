# rollup-plugin-rootsqz
![NPM Version](https://img.shields.io/npm/v/rollup-plugin-rootsqz)

Rollup / Vite plugin for using [rootsqz](https://github.com/r00tkids/rootsqz) to compress and bundle code and assets into one HTML file. This is intented for intros in the [demoscene](https://en.wikipedia.org/wiki/Demoscene) or size restricted JS challenges.

## Install
`npm i rollup-plugin-rootsqz`

## Usage
```js
// vite.config.js
import { defineConfig } from 'vite';
import rootsqz from 'rollup-plugin-rootsqz';

export default defineConfig({
  plugins: [rootsqz()]
});
```

See the [example](https://github.com/r00tkids/rootsqz/tree/main/rollup-plugin-rootsqz/example) for a working example with support for `vite-plugin-glsl`.

## Example Options
```js
rootsqz({
    /*
    Full path to the rootsqz executable (currently named `websqz`).
    If null (default), the plugin uses the executable
    installed when installing the npm package.
    Otherwise it will try to resolve `websqz` from your system PATH.
    */
    rootsqzPath: null,
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