export interface Attachment { id: string; name: string; mime: string; data: string }
const START = '<perpetual-attachments>';
const END = '</perpetual-attachments>';
export const MAX_BYTES = 20 * 1024 * 1024;
export function unpackMessage(message: string): { text: string; attachments: Attachment[] } {
  const start = message.indexOf(START);
  if (start < 0) return { text: message, attachments: [] };
  const end = message.lastIndexOf(END);
  if (end < 0) return { text: message, attachments: [] };
  try {
    const attachments = JSON.parse(message.slice(start + START.length, end)) as Attachment[];
    if (!Array.isArray(attachments) || !attachments.every(a => typeof a.name === 'string' && typeof a.data === 'string' && typeof a.mime === 'string')) throw new Error();
    return { text: message.slice(0, start) + message.slice(end + END.length), attachments };
  } catch { return { text: message, attachments: [] }; }
}
export function packMessage(text: string, attachments: Attachment[]): string {
  return attachments.length ? text.trim() + '\n\n' + START + JSON.stringify(attachments) + END : text;
}
export function displayMessage(message: string): string {
  const {text, attachments} = unpackMessage(message);
  return [text, ...attachments.map(a => `📎 ${a.name}`)].filter(Boolean).join('\n');
}
export function attachmentBytes(attachments: Attachment[]): number {
  return attachments.reduce((n,a) => n + Math.floor(a.data.length * 3 / 4) - (a.data.endsWith('==') ? 2 : a.data.endsWith('=') ? 1 : 0), 0);
}
export async function readAttachments(files: File[]): Promise<Attachment[]> {
  if (files.some(f => f.size > MAX_BYTES) || files.reduce((n,f) => n + f.size, 0) > MAX_BYTES) throw new Error('Attachments must total 20 MB or less.');
  if (files.some(f => f.type.startsWith('image/') && f.size > 5 * 1024 * 1024)) throw new Error('Images must be 5 MB or less.');
  return Promise.all(files.map(file => new Promise<Attachment>((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error(`Could not read ${file.name}`));
    reader.onload = () => resolve({ id: crypto.randomUUID(), name: file.name || 'Clipboard image.png', mime: file.type || (file.name.toLowerCase().endsWith('.pdf') ? 'application/pdf' : 'application/octet-stream'), data: String(reader.result).split(',')[1] });
    reader.readAsDataURL(file);
  })));
}
