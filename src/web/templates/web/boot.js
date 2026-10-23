{{#if boot_js}}
// --boot-js and --title, before anything else
(() => {
{{{boot_js}}}
})();
{{/if}}
{{#if worker}}
// The decoder runs in a web worker, made from the source text of this function
let w = new Worker(URL.createObjectURL(new Blob(["(" + (() => {
{{{decompressor_source}}}
{{#if wasm}}
onmessage = e => decompressWorker(e.data);
{{else}}
onmessage = e => decompressWorker(model, e.data, {{{encoded_len}}}, {{{decoded_len}}});
{{/if}}
}) + ")()"])));
{{else}}
{{{decompressor_source}}}
{{/if}}

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
    window.eval({{{main_js}}});
};
p.slice(o).arrayBuffer().then(b => {
    a = new Uint8Array(b);
{{#if worker}}
    // The worker posts the number of decoded bytes, and then the decoded bytes
    w.onmessage = e => e.data.at ? run(e.data) : rsqz.progress?.(e.data, {{{decoded_len}}});
    w.postMessage(a);
{{else}}
    // Decode after the loader has been painted: a requestAnimationFrame callback runs before the paint, the timeout after it
    requestAnimationFrame(() => setTimeout(() =>
{{#if wasm}}
{{#if sliced}}
        decompressSliced(a, run)
{{else}}
        decompress(a).then(run)
{{/if}}
{{else if sliced}}
        decompressSliced(model, a, {{{encoded_len}}}, {{{decoded_len}}}, run)
{{else}}
        run(decompress(model, a, {{{encoded_len}}}, {{{decoded_len}}}))
{{/if}}
    ));
{{/if}}
});
{{else}}
document.body.innerHTML = "";
p.slice(o).arrayBuffer().then(b => {
    a = new Uint8Array(b);
{{#if worker}}
    w.postMessage(a);
    // The worker posts the number of decoded bytes, and then the decoded bytes
    return new Promise(r => w.onmessage = e => e.data.at && r(e.data));
}).then(d => {
{{else if sliced}}
    // The sliced decoder reports progress to rsqz.progress
    rsqz = {};
{{#if wasm}}
    return new Promise(r => decompressSliced(a, r));
{{else}}
    return new Promise(r => decompressSliced(model, a, {{{encoded_len}}}, {{{decoded_len}}}, r));
{{/if}}
}).then(d => {
{{else if wasm}}
    return decompress(a);
}).then(d => {
{{else}}
    d = decompress(model, a, {{{encoded_len}}}, {{{decoded_len}}});
{{/if}}
    rsqz = {
        files: {{{files_map}}}
    };
    s = {{{main_js}}};
    window.eval(s);
});
{{/if}}
