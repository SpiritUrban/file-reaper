/**
 * Копіювання тексту в буфер обміну. `navigator.clipboard` у WebView2 працює
 * лише в secure context і при фокусі; тому — надійний фолбек через прихований
 * `<textarea>` + `execCommand("copy")`, який спрацьовує синхронно в обробнику
 * кліку. Повертає true при успіху.
 */
export async function copyToClipboard(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // падаємо у фолбек нижче
  }

  try {
    const textarea = document.createElement("textarea");
    textarea.value = text;
    textarea.style.position = "fixed";
    textarea.style.opacity = "0";
    textarea.style.pointerEvents = "none";
    document.body.appendChild(textarea);
    textarea.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(textarea);
    return ok;
  } catch {
    return false;
  }
}
