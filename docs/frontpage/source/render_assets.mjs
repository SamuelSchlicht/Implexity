// SPDX-License-Identifier: Apache-2.0
// npm install playwright; npx playwright install chromium; node render_assets.mjs
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';
const dir=path.dirname(fileURLToPath(import.meta.url));
const browser=await chromium.launch({headless:true});
const page=await browser.newPage({viewport:{width:1200,height:900},deviceScaleFactor:2});
for(const [input,output] of [['geometry','geometry'],['scaffold','scaffold-study'],['divertor','divertor-study'],['geometry-mobile','geometry-mobile'],['scaffold-mobile','scaffold-study-mobile'],['divertor-mobile','divertor-study-mobile']]){
  await page.goto(pathToFileURL(path.join(dir,input+'.html')).href);
  await page.evaluate(async()=>{await document.fonts.ready;await Promise.all([...document.images].map(i=>i.decode()));});
  await page.locator('.canvas').screenshot({path:path.resolve(dir,'../../assets/'+output+'.png')});
  console.log(output);
}
await browser.close();
