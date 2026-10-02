import smallWasm from "./assets/plasma.wasm?websqz-bin&raw";

const width = 128;
const height = 128;

const memory = new WebAssembly.Memory({ initial: 1 });
const imports = {
  math: {
    sin: Math.sin,
    cos: Math.cos,
    sqrt: Math.sqrt,
    floor: Math.floor,
  },
  image: {
    memory,
  }
};

(async () => {
  const { instance } = await WebAssembly.instantiate(smallWasm, imports);

  if (!instance.exports.render) {
    console.error('No `render` export found in wasm');
  }

  const bytes = new Uint8Array(memory.buffer, 0, width * height * 4);
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext('2d');

  if (!document.body.contains(canvas)) {
    document.body.appendChild(canvas);
  }

  function animate(t) {
    instance.exports.render(t | 0);

    const imageData = ctx.createImageData(width, height);
    imageData.data.set(bytes);
    ctx.putImageData(imageData, 0, 0);

    requestAnimationFrame(animate);
  }

  animate();
})();
