// Watching a directory, as a build tool does.
const fs = require('fs');

const watcher = fs.watch('.', () => {});
setTimeout(() => {
  watcher.close();
  console.log('watched');
}, 50);
