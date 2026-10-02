document.body.innerHTML = "<h1>Hello, RootSQZ!</h1>";

let glsl = document.createElement("p").textContent = new TextDecoder().decode(wsqz.files["bundled.glsl"]);
console.log("GLSL content:", glsl);