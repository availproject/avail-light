import init, { run, post_message } from './avail_light_web.js';

(async () => {
  try {
    await init();
    await run("hex", null);
  } catch (error) {
    console.error("Error:", error);
  }
})();

self.onmessage = (e) => {
  post_message(JSON.stringify(e.data));
};
