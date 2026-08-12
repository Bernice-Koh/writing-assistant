// Office.onReady resolving is what separates a task pane Word actually initialised from a
// page that merely rendered in the pane frame.
Office.onReady((info) => {
  const status = document.getElementById("status");
  if (status) {
    status.textContent = `Office ready. Host: ${info.host}, platform: ${info.platform}.`;
  }
});
