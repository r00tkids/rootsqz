let bar = document.querySelector("#L *");
rsqz.progress = (done, total) => bar.style.width = done / total * 100 + "%";
