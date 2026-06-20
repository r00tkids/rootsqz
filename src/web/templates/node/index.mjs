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

    fs.writeFileSync(__dirname + '/{{{output_file}}}', decompress(model, new Uint8Array(data.buffer), {{{encoded_len}}}, {{{decoded_len}}}));
});

