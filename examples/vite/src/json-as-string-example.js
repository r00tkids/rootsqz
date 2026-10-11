// Imports a JSON file as a string, not as an object.
// A plain `import demo from "./assets/demo.json"` gives an object, which packs smaller. Import the
// JSON as a string only when code needs the text, e.g. a library that parses it itself.
// `&raw` keeps Vite's own JSON handling away from the import, without it the build fails.
// The file is packed as it is, so keep it minified.
import demoJsonString from "./assets/demo.json?rootsqz-txt&raw";

console.log("JSON as a string:", typeof demoJsonString, demoJsonString);
console.log("Parsed:", JSON.parse(demoJsonString));
