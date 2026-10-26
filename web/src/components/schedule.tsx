import { useT } from "@/i18n"
import { describeCron } from "@/lib/cron"

/** A cron expression in words; as written when it has no words. */
export function Schedule({ cron }: { cron: string }) {
  const t = useT()
  const words = describeCron(t, cron)
  if (!words) {
    return (
      <code className="type-mono" translate="no">
        {cron}
      </code>
    )
  }
  return <span>{words}</span>
}
