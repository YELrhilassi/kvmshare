import type { Page } from "@/app/nav";
import RolePicker from "@/features/home/RolePicker";
import ShareStatus from "@/features/home/ShareStatus";
import ConnectInfo from "@/features/home/ConnectInfo";
import LiveOverview from "@/features/home/LiveOverview";
import Updater from "@/features/home/Updater";

// The dashboard: what this machine is doing right now (role, live state,
// who is connected, what is on the network) and the single start/stop
// control. Everything here is state; settings live on their own pages.
export default function HomePage(_props: { onNavigate: (p: Page) => void }) {
  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-5xl px-10 py-14">
        <header className="mb-12 space-y-1">
          <h1 className="text-2xl font-semibold tracking-tight">This machine</h1>
          <p className="text-sm text-muted-foreground">
            What it's doing right now — and who else is around.
          </p>
        </header>

        <div className="space-y-14">
          <RolePicker />

          <div className="grid gap-x-16 gap-y-12 lg:grid-cols-2">
            <ShareStatus />
            <ConnectInfo />
          </div>

          <LiveOverview />

          <Updater />
        </div>
      </div>
    </div>
  );
}