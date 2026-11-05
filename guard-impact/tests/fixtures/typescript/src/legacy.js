const money = require("./lib/money");
module.exports = { price: (c) => money.fmt(c) };
