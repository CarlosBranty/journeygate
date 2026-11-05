import "./components";

export async function loadPay() {
  const { Pay } = await import("./checkout/pay");
  return Pay;
}
