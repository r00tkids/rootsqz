{{{decompressor_source}}}

{{#if loader}}
rsqz = {};
document.body.innerHTML = {{{loader_html}}};
{{#if loader_js}}
(() => {
{{{loader_js}}}
})();
{{/if}}
// Runs the main JavaScript with the decoded bytes
let run = d => {
    rsqz.files = {{{files_map}}};
    {{#unless keep}}
    document.body.innerHTML = "";
    {{/unless}}
    window.eval(new TextDecoder().decode(d.slice(0, {{{js_main_len}}})));
};
p.slice(o).arrayBuffer().then(b => {
    a = new Uint8Array(b);
    // Decode after the loader has been painted: a requestAnimationFrame callback runs before the paint, the timeout after it
    requestAnimationFrame(() => setTimeout(() =>
{{#if loader_js}}
        decompressSliced(model, a, {{{encoded_len}}}, {{{decoded_len}}}, run)
{{else}}
        run(decompress(model, a, {{{encoded_len}}}, {{{decoded_len}}}))
{{/if}}
    ));
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
