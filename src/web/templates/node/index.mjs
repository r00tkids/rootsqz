import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = import.meta.dirname;

{{{decompressor_source}}}

fs.readFile(__dirname + '/{{{input_file}}}', (err, data) => {
    if (err) {
        console.error('Error reading file:', err);
        return;
    }

{{#if wasm}}
    decompress(new Uint8Array(data.buffer)).then(d => {
{{else}}
    let d = decompress(model, new Uint8Array(data.buffer), {{{encoded_len}}}, {{{decoded_len}}});
{{/if}}
{{#if restore}}
    d = ({{{restore}}})(d);
{{/if}}
    fs.writeFileSync(__dirname + '/{{{output_file}}}', d);
{{#if wasm}}
    });
{{/if}}
});

