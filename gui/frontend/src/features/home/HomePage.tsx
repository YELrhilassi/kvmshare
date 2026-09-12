import RolePicker from "@/features/home/RolePicker";
import StatusHero from "@/features/home/StatusHero";
import ConnectInfo from "@/features/home/ConnectInfo";
import LiveOverview from "@/features/home/LiveOverview";
import Updater from "@/features/home/Updater";

// The dashboard: the hero answers "what is happening right now" with
// the live desk map beside it; below, the one role decision, the facts
// another machine needs, the network, and the updater. Everything here
// is state; settings live on their own pages.
export default function HomePage() {
  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-5xl px-10 py-12">
        <StatusHero />

        <div className="mt-12 grid gap-x-10 gap-y-12 lg:grid-cols-2">
          <RolePicker />
          <ConnectInfo />
        </div>

        <div className="mt-14">
          <LiveOverview />
        </div>

        <div className="mt-14">
          <Updater />
        </div>
      </div>
    </div>
  );
}
