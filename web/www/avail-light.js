import init, { run } from './avail_light_web.js';

(async () => {
    try {
        await init();
        await run("hex", null);
    } catch (error) {
        console.error("Error:", error);
    }
})();
