import init, { latest_block } from './avail_light_web.js';

(async () => {
  try {
    await init();
    console.log(await latest_block("hex"));
  } catch (error) {
    console.error("Error:", error);
  }
})();
