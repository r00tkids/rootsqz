{{{decompressor_source}}}

{{#if loader}}
rsqz = {};
document.body.innerHTML = {{{loader_html}}};
{{#if loader_js}}
(() => {
{{{loader_js}}}
})();
rsqz.progress?.(0, {{{decoded_len}}});
{{/if}}
p.slice(o).arrayBuffer().then(b => {
    a = new Uint8Array(b);
    // Decode after the loader has been painted: a requestAnimationFrame callback runs before the paint, the timeout after it
    requestAnimationFrame(() => setTimeout({{#if loader_js}}async {{/if}}() => {
        d = {{#if loader_js}}await decompressSliced{{else}}decompress{{/if}}(model, a, {{{encoded_len}}}, {{{decoded_len}}});
        rsqz.files = {{{files_map}}};
        {{#unless keep}}
        document.body.innerHTML = "";
        {{/unless}}
        s = new TextDecoder().decode(d.slice(0, {{{js_main_len}}}));
        window.eval(s);
    }));
});
{{else}}
document.body.innerHTML = "";
p.slice(o).arrayBuffer().then(b => {
    a = new Uint8Array(b);
    d = decompress(model, a, {{{encoded_len}}}, {{{decoded_len}}});
    rsqz = {
        files: {{{files_map}}}
    };
    s = new TextDecoder().decode(d.slice(0, {{{js_main_len}}}));
    window.eval(s);
});
{{/if}}
