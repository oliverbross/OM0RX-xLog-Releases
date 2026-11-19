import { Check, CircleCheck, Minus } from "lucide-react";
import "./ChaseAlerts.css";

export type ProgressState = "needed" | "worked" | "confirmed" | "unknown";
const labels = { needed: "Needed", worked: "Worked", confirmed: "Confirmed", unknown: "Unknown" };
const icon = (state: ProgressState) => state === "confirmed" ? <CircleCheck size={13} aria-hidden="true"/> : state === "worked" ? <Check size={13} aria-hidden="true"/> : <Minus size={13} aria-hidden="true"/>;
export function BandModeProgressTable({ label, bands, modes, state, confirmations = false }: {
  label: string; bands: string[]; modes: string[]; state: (band: string, mode: string) => ProgressState; confirmations?: boolean;
}) {
  return <>
    <div className="chase-matrix-scroll"><table className="chase-matrix" aria-label={label}>
      <thead><tr><th scope="col">Band</th>{modes.map(mode => <th scope="col" key={mode}>{mode}</th>)}</tr></thead>
      <tbody>{bands.map(band => <tr key={band}><th scope="row">{band}</th>{modes.map(mode => {
        const value = state(band, mode), title = `${band} ${mode}: ${labels[value]}`;
        return <td key={mode} className={`is-${value}`} aria-label={title} title={title}>{icon(value)}</td>;
      })}</tr>)}</tbody>
    </table></div>
    <div className="chase-matrix-legend">{(["needed", "worked", ...(confirmations ? ["confirmed"] : [])] as ProgressState[]).map(value => <span key={value}>{icon(value)}{labels[value]}</span>)}</div>
  </>;
}
