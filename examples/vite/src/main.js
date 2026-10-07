import rootsqzJpg from "./assets/rootsqz.jpg?rootsqz-bin&compressed&raw";
import binary from "./assets/rand.bin?rootsqz-bin";
import json from "./assets/test.json?rootsqz-txt&raw";
import "./wasm-example.js";
import frag from "./assets/frag.glsl";

if (import.meta.hot) {
  import.meta.hot.accept(["./assets/frag.glsl", "./assets/rand.bin?rootsqz-bin", "./assets/test.json?rootsqz-txt&raw"], (modules) => {
    for (const mod of modules) {
      if (!mod) continue; // module not updated
      console.log("HMR update:", mod.default);
    }
  });
}

console.log("Fragment Shader:", frag);
console.log("Binary Data:", binary);
console.log("Test JSON:", json);

let blob = new Blob([rootsqzJpg], { type: 'image/jpeg' });
let url = URL.createObjectURL(blob);
let img = document.createElement('img');
img.src = url;
document.body.appendChild(img);
