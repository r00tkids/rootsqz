import rootsqzJpg from "./assets/rootsqz.jpg?rootsqz-bin&pre-compressed&raw";
import binary from "./assets/rand.bin?rootsqz-bin";
import scroller from "./assets/scroller.txt?rootsqz-txt";
import "./wasm-example.js";
import "./json-as-string-example.js";
import frag from "./assets/frag.glsl";

if (import.meta.hot) {
  import.meta.hot.accept(["./assets/frag.glsl", "./assets/rand.bin?rootsqz-bin", "./assets/scroller.txt?rootsqz-txt"], (modules) => {
    for (const mod of modules) {
      if (!mod) continue; // module not updated
      console.log("HMR update:", mod.default);
    }
  });
}

console.log("Fragment Shader:", frag);
console.log("Binary Data:", binary);
console.log("Scroller:", scroller);

let blob = new Blob([rootsqzJpg], { type: 'image/jpeg' });
let url = URL.createObjectURL(blob);
let img = document.createElement('img');
img.src = url;
document.body.appendChild(img);
