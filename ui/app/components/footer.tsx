import { MoveUpRight, MessageSquare } from "lucide-react";
import { useState } from "react";
import ContactUsSheet from "./ContactUsSheet";

const FooterComponent = () => {
  
  const [isOpen, setIsOpen] = useState(false);

  return (
    <div className="h-full w-full border-t flex flex-col md:flex-row items-center justify-between gap-2 md:gap-0 px-4 py-2 md:py-0 bg-[#F7F3EF]">
      <div className="flex flex-wrap h-full items-center justify-center md:justify-start gap-x-4 gap-y-1 text-gray-600 text-xs md:text-sm">
        <a
          href="https://github.com/FalkorDB/benchmark/blob/master/readme.md#data"
          className="hover:text-gray-800 flex items-center whitespace-nowrap"
          target="_blank"
        >
          DATASET USED <MoveUpRight size={16} />
        </a>
        <span className="hidden md:inline border-l-2 border-gray-300 h-2/4 self-center"></span>
        <a
          href="https://github.com/FalkorDB/benchmark/blob/master/readme.md"
          className="hover:text-gray-800 flex items-center whitespace-nowrap"
          target="_blank"
        >
          README <MoveUpRight size={16} />
        </a>
        <span className="hidden md:inline border-l-2 border-gray-300 h-2/4 self-center"></span>

        <a
          href="https://github.com/FalkorDB/benchmark/blob/master/readme.md#faq"
          className="hover:text-gray-800 flex items-center whitespace-nowrap"
          target="_blank"
        >
          FAQ <MoveUpRight size={16} />
        </a>
        <span className="hidden md:inline border-l-2 border-gray-300 h-2/4 self-center"></span>
        <a
          href="https://github.com/FalkorDB/benchmark/blob/master/readme.md#installation-steps"
          className="hover:text-gray-800 flex items-center whitespace-nowrap"
          target="_blank"
        >
          RUN THE BENCHMARK <MoveUpRight size={16} />
        </a>
      </div>

      <div className="fixed bottom-4 left-4 z-40 md:static md:z-auto flex items-center w-auto md:w-auto bg-transparent md:bg-muted/50 md:p-4">
        <button
          onClick={() => setIsOpen(true)}
          aria-label="Speak with us"
          title="Speak with us"
          className="bg-[#FF66B3] text-[#ffffff] border shadow-lg md:shadow size-14 md:size-auto rounded-full md:rounded-lg p-0 md:px-4 md:py-2 font-semibold md:min-w-[150px] md:min-h-[40px] whitespace-nowrap text-center flex items-center justify-center gap-2 hover:opacity-90 transition-opacity"
        >
          <MessageSquare className="size-6 md:hidden" />
          <span className="hidden md:inline">SPEAK WITH US</span>
        </button>
        <ContactUsSheet isOpen={isOpen} setIsOpen={setIsOpen} />
      </div>
    </div>
  );
};

export default FooterComponent;
