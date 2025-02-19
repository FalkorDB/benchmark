import { HoverCard, HoverCardContent, HoverCardTrigger } from "@/components/ui/hover-card";

export function HardwareInfo({ cpu, ram, storage }: { cpu: string; ram: string; storage: string }) {
  return (
    <HoverCard>
      <HoverCardTrigger>
        <span className="inline-block w-4 h-4 bg-gray-400 text-white rounded-full text-center text-xs font-bold cursor-pointer">
          i
        </span>
      </HoverCardTrigger>
      <HoverCardContent className="bg-gray-50 text-gray-900 p-4 rounded-lg shadow-md max-w-sm border border-gray-200">
        <div className="space-y-2">
          <p className="text-sm">
            <strong className="font-semibold text-gray-700">CPU:</strong> {cpu}
          </p>
          <p className="text-sm">
            <strong className="font-semibold text-gray-700">RAM:</strong> {ram}
          </p>
          <p className="text-sm">
            <strong className="font-semibold text-gray-700">Storage:</strong> {storage}
          </p>
        </div>
      </HoverCardContent>
    </HoverCard>
  );
}
