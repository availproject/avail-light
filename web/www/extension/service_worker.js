import init, { latest_block } from './avail_light_web.js';

let initialized = false;

const network = 'hex'; // 'local', 'hex' or 'mainnet'

async function ensureInit() {
  if (initialized) {
	return;
  }
  await init();
  initialized = true;
}

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  switch (message.action) {
    case 'latest_block': {
	  (async () => {
		try {
		  await ensureInit();
		  sendResponse({ result: await latest_block(network)});
		} catch (e) {
		  console.error('latest_block error:', e);
		  sendResponse({ error: e.toString() });
		}
	  })();
      return true; // keep message channel open for async sendResponse
    }
    default:
      return false;
  }
});
