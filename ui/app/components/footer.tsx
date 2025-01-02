import { MoveUpRight } from "lucide-react";

const FooterComponent = () => {
  return (
    <div className="h-full w-full border-t flex items-center justify-between px-4 bg-[#F7F3EF]">
      <div className="flex h-full items-center space-x-4 text-gray-600 text-sm">
        <a
          href="#"
          className="hover:text-gray-800 flex items-center h-full whitespace-nowrap"
        >
          DATASET USED <MoveUpRight size={16} />
        </a>
        <span className="border-l-2 border-gray-300 h-2/4 self-center"></span>

        <a
          href="#"
          className="hover:text-gray-800 flex items-center h-full whitespace-nowrap"
        >
          HOW TO INSTALL <MoveUpRight size={16} />
        </a>
        <span className="border-l-2 border-gray-300 h-2/4 self-center"></span>

        <a
          href="#"
          className="hover:text-gray-800 flex items-center h-full whitespace-nowrap"
        >
          README <MoveUpRight size={16} />
        </a>
        <span className="border-l-2 border-gray-300 h-2/4 self-center"></span>

        <a
          href="#"
          className="hover:text-gray-800 flex items-center h-full whitespace-nowrap"
        >
          FAQ <MoveUpRight size={16} />
        </a>
      </div>

      <div className="flex items-center h-16 w-full bg-muted/50 p-4">
        <button className="bg-pink-500 text-white px-4 py-2 rounded-lg font-semibold text-sm hover:bg-pink-600 min-w-[150px] max-w-full min-h-[40px] max-h-full whitespace-normal break-words ml-auto">
          RUN THE BENCHMARK WITH YOUR DATA
        </button>
      </div>
    </div>
  );
};

export default FooterComponent;
