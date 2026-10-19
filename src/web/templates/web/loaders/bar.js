rsqz.progress = (done, total) => {
    document.querySelector("#rsqz-loader div").style.width = done / total * 100 + "%";
};
