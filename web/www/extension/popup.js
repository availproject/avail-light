setTimeout(async () => {
  const response = await chrome.runtime.sendMessage({ action: 'latest_block' });
  document.getElementById("response").innerText = response.result || response.error;
}, 1000);
