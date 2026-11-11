import type { ContestSetupDocument, ContestSetupMacro } from "./ContestSetupPanel";
import { contestCapabilities, contestExchangeSample, contestPreflight, contestReadinessEvidence, type ContestPreflightContext } from "./contestReadiness";
import "./ContestReadinessPanel.css";

const sponsorRules: Record<string, string> = {
  cq_ww_cw: "https://www.cqww.com/rules/", cq_ww_ssb: "https://www.cqww.com/rules/",
  cq_wpx_cw: "https://cqwpx.com/rules/", cq_wpx_ssb: "https://cqwpx.com/rules/",
};

export function ContestReadinessPanel({ document, context, macros }: { document: ContestSetupDocument; context?: ContestPreflightContext; macros?: ContestSetupMacro[] }) {
  return <details className="contest-readiness"><summary>Readiness and preflight · {context?.practice ? "Practice" : "review before operating"}</summary>
    <p>{document.settings.OperatorCategory} · {document.settings.BandCategory} · {document.settings.ModeCategory} · {document.settings.PowerCategory}</p>
    <p>Sample sent/logged fields: {contestExchangeSample(document)}. This is a field example; no message is sent.</p>
    <p>Evidence: {contestReadinessEvidence.date} · {contestReadinessEvidence.scope}.</p>
    {document.pluginID === "cqws" && <p role="alert">CQWS scoring remains partial/unverified for Brazilian UF branches. RTC score posting is restricted.</p>}
    <dl>{[...contestCapabilities(document.pluginID, document.settings), ...contestPreflight(document, context, macros)].map(item => <div key={item.label}><dt>{item.label} <strong>{item.status}</strong></dt><dd>{item.detail}</dd></div>)}</dl>
    {sponsorRules[document.pluginID] && <p><a href={sponsorRules[document.pluginID]} target="_blank" rel="noreferrer">Organizer’s rules for this contest</a></p>}
    <p>Accepted evidence covers its stated fixtures. Verify the current organizer rules, category and physical station before operating. Rules/reference updates remain deliberate; historical sessions retain their policy.</p>
  </details>;
}
