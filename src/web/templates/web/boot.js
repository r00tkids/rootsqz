{{{decompressor_source}}}

document.body.innerHTML = "";
p.slice(o).arrayBuffer().then(b => {
    a = new Uint8Array(b);
    d = decompress(model, a, {{{encoded_len}}}, {{{decoded_len}}});
    rsqz = {
        {{{files_map}}}
    };
    s = new TextDecoder().decode(d.slice(0, {{{js_main_len}}}));
    eval(s);
});
